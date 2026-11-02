//! `cargo xtask verify`: positive and negative QEMU boot cases.

use crate::config::Tools;
use crate::image::{self, Mode};
use crate::markers::Expectation;
use crate::qemu::{self, Action, Interaction, Outcome, Usb};
use crate::screen;
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;
use zoo_abi::font::FaceId;
use zoo_abi::ui::{self as zui, LIGHT};

/// Wait this long for quiet serial output before a screenshot.
const QUIET: Duration = Duration::from_millis(1200);
/// Long enough for the auto-hidden dock to slide in (docs/u2e-design.md).
const DOCK_SLIDE: Duration = Duration::from_millis(500);

/// Must equal the kernel crate version, which the kernel prints at boot.
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct Settings {
    /// Report the inference case as not run when the model is missing.
    pub skip_model: bool,
    /// Run only the case with this name (for iterating; the full set is the
    /// acceptance run).
    pub only: Option<String>,
    pub timeout: Duration,
    pub negative_timeout: Duration,
    pub settle: Duration,
}

/// Inspects a screenshot; `Ok` carries a summary, `Err` why it failed.
type ScreenCheck = fn(&screen::Image) -> Result<String, String>;

thread_local! {
    /// The screenshot checked just before the current one in the same case,
    /// for checks that compare two pictures (`check_full_repaint`).
    static PREVIOUS: std::cell::RefCell<Option<screen::Image>> =
        const { std::cell::RefCell::new(None) };
    /// When the screenshot being checked was written (its file time), for
    /// checks of the guest's clock (`check_menu_clock`).
    static SHOT_TIME: std::cell::Cell<Option<std::time::SystemTime>> =
        const { std::cell::Cell::new(None) };
}

/// Which data disk a case boots with.
#[derive(Clone, Copy)]
enum Disk {
    /// An empty disk made for this case.
    Fresh,
    /// `build/disks/<name>.img`, shared by consecutive cases (two boots of
    /// one machine). `reset` empties it first; `corrupt` damages the newest
    /// snapshot's payload before booting.
    Shared {
        name: &'static str,
        reset: bool,
        corrupt: bool,
    },
}

struct Case {
    name: &'static str,
    mode: Mode,
    /// Boot this image instead of the mode's own (the model image).
    custom_image: Option<std::path::PathBuf>,
    /// The data disk for this run (docs/s5-design.md).
    disk: Disk,
    /// Attach a USB controller with a keyboard and a mouse
    /// (docs/h1-design.md). Off by default: QEMU could then route typed
    /// keys to the USB keyboard instead of the PS/2 one.
    usb: Usb,
    expectation: Expectation,
    timeout: Duration,
    /// The outcome kind this case must produce for verification to succeed.
    want: &'static str,
    purpose: &'static str,
    /// Keys to type and whether to take a screenshot (keyboard cases).
    interaction: Option<Interaction>,
    /// Checks applied to named screenshots; any failure fails the case.
    screen_checks: Vec<(&'static str, ScreenCheck)>,
}

const SELF_TESTS: [&str; 6] = [
    "ZOO_OS TEST breakpoint ok",
    "ZOO_OS TEST frames ok",
    "ZOO_OS TEST paging ok",
    "ZOO_OS TEST address-spaces ok",
    "ZOO_OS TEST heap ok",
    "ZOO_OS TEST timer ok",
];

/// The M3 start-up scenario (docs/m3-design.md §9). Every line is printed by
/// the kernel, and Zookeeper's sequential plan fixes their order.
const BOOT_PROCESSES: [&str; 8] = [
    "ZOO_OS PROC start pid=1 name=zookeeper",
    "ZOO_OS PROC start pid=2 name=beluga-test",
    "ZOO_OS PROC start pid=3 name=lemming-test",
    "ZOO_OS PROC crash pid=3 name=lemming-test fault=page-fault access=read cause=protection addr=0xffffc00000001000",
    "ZOO_OS PROC start pid=4 name=lemming-test",
    "ZOO_OS PROC crash pid=4 name=lemming-test fault=page-fault access=read cause=protection addr=0xffffc00000001000",
    "ZOO_OS PROC start pid=5 name=muskox-test",
    "ZOO_OS PROC killed pid=5 name=muskox-test by=1",
];

/// M5a: two BeaverHandlers start in order and finish concurrently.
const BEAVERS_START: [&str; 2] = [
    "ZOO_OS PROC start pid=6 name=narwhal-test",
    "ZOO_OS PROC start pid=7 name=narwhal-test",
];
const BEAVERS_EXIT: [&str; 2] = [
    "ZOO_OS PROC exit pid=6 name=narwhal-test code=0",
    "ZOO_OS PROC exit pid=7 name=narwhal-test code=0",
];
/// S4b: Zookeeper's boot checks use a private BadgerHandler (filesystem
/// service), started before the file tests and stopped after them.
const BADGER_START: [&str; 1] = ["ZOO_OS PROC start pid=8 name=badger-handler"];
const BADGER_EXIT: [&str; 1] = ["ZOO_OS PROC exit pid=8 name=badger-handler code=0"];
/// POSIX step 4: ArcticFoxTest's writable-file checks.
const ARCTICFOX: [&str; 2] = [
    "ZOO_OS PROC start pid=9 name=arcticfox-test",
    "ZOO_OS PROC exit pid=9 name=arcticfox-test code=0",
];
/// M5c-1: OtterHandler (the action broker) answers Zookeeper's script.
const OTTER: [&str; 2] = [
    "ZOO_OS PROC start pid=10 name=otter-handler",
    "ZOO_OS PROC exit pid=10 name=otter-handler code=0",
];
/// POSIX step 1: CaribouTest's thread test.
const ANT: [&str; 2] = [
    "ZOO_OS PROC start pid=11 name=caribou-test",
    "ZOO_OS PROC exit pid=11 name=caribou-test code=0",
];
/// POSIX step 2: WalrusTest's address-space checks; four children must
/// crash with exactly these faults.
const MOLE: [&str; 10] = [
    "ZOO_OS PROC start pid=12 name=walrus-test",
    "ZOO_OS PROC start pid=13 name=walrus-test",
    "ZOO_OS PROC crash pid=13 name=walrus-test fault=page-fault access=read cause=not-present addr=0x0000000040000000",
    "ZOO_OS PROC start pid=14 name=walrus-test",
    "ZOO_OS PROC crash pid=14 name=walrus-test fault=page-fault access=write cause=protection addr=0x0000000040000000",
    "ZOO_OS PROC start pid=15 name=walrus-test",
    "ZOO_OS PROC crash pid=15 name=walrus-test fault=page-fault access=execute cause=protection addr=0x0000000040000000",
    "ZOO_OS PROC start pid=16 name=walrus-test",
    "ZOO_OS PROC crash pid=16 name=walrus-test fault=page-fault access=write cause=not-present addr=0x0000000040000000",
    "ZOO_OS PROC exit pid=12 name=walrus-test code=0",
];
/// POSIX step 3: WolverineTest's exception checks; its children must end
/// exactly like this (unhandled fault, fault inside the handler, refused
/// return context).
const GECKO: [&str; 8] = [
    "ZOO_OS PROC start pid=17 name=wolverine-test",
    "ZOO_OS PROC start pid=18 name=wolverine-test",
    "ZOO_OS PROC crash pid=18 name=wolverine-test fault=page-fault access=write cause=not-present addr=0x0000000000000010",
    "ZOO_OS PROC start pid=19 name=wolverine-test",
    "ZOO_OS PROC crash pid=19 name=wolverine-test fault=page-fault access=read cause=not-present addr=0x0000000000000020",
    "ZOO_OS PROC start pid=20 name=wolverine-test",
    "ZOO_OS PROC exit pid=20 name=wolverine-test code=103",
    "ZOO_OS PROC exit pid=17 name=wolverine-test code=0",
];
/// Services step 3: ptarmigan-test's handle-passing checks.
const PTARMIGAN: [&str; 2] = [
    "ZOO_OS PROC start pid=21 name=ptarmigan-test",
    "ZOO_OS PROC exit pid=21 name=ptarmigan-test code=0",
];
/// The resident supervisor and its services start before READY
/// (docs/s1-design.md).
const BOOT_PROCESSES_END: [&str; 9] = [
    "ZOO_OS PROC exit pid=2 name=beluga-test code=0",
    "ZOO_OS PROC exit pid=1 name=zookeeper code=0",
    "ZOO_OS TEST process-cleanup ok",
    "ZOO_OS PROC start pid=22 name=zookeeper",
    "ZOO_OS PROC start pid=23 name=badger-handler",
    "ZOO_OS PROC start pid=24 name=beaver-handler",
    "ZOO_OS PROC start pid=25 name=seal-test",
    "ZOO_OS READY",
    "ZOO_OS SHELL ready",
];

fn with_tests(last: &str) -> Vec<&str> {
    let mut v = SELF_TESTS.to_vec();
    v.push(last);
    v
}

fn expect(mode: Mode, tail: &[&str]) -> Expectation {
    Expectation::new(VERSION, mode.kernel_mode(), tail)
}

pub fn run(root: &Path, tools: &Tools, s: &Settings) -> Result<ExitCode, String> {
    // The window server's font, for finding its text in screenshots.
    screen::set_atlas(crate::font::build_atlas(root)?);
    let mut images = Vec::new();
    for mode in Mode::ALL {
        images.push((mode, image::build(root, tools, mode)?));
    }

    let mut head = SELF_TESTS.to_vec();
    head.extend(BOOT_PROCESSES);
    head.extend(BEAVERS_START);
    let boot = |mode: Mode| {
        let mut e = expect(mode, &head);
        e.then_any_order(&BEAVERS_EXIT)
            .then(&BADGER_START)
            .then(&ARCTICFOX)
            .then(&OTTER)
            .then(&BADGER_EXIT)
            .then(&ANT)
            .then(&MOLE)
            .then(&GECKO)
            .then(&PTARMIGAN)
            .then(&BOOT_PROCESSES_END);
        e
    };
    let normal = boot(Mode::Normal);
    let mut big_screen = boot(Mode::BigScreen);
    big_screen.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=assistant",
    ]);
    // B1: the same system without the boot checks. D6: it starts in the
    // desktop; after Escape the shell runs a C program.
    let mut fast_head = SELF_TESTS.to_vec();
    fast_head.extend([
        "ZOO_OS PROC start pid=1 name=zookeeper",
        "ZOO_OS PROC start pid=2 name=badger-handler",
        "ZOO_OS PROC start pid=3 name=beaver-handler",
        "ZOO_OS PROC start pid=4 name=seal-test",
        "ZOO_OS READY",
        "ZOO_OS SHELL ready",
        "ZOO_OS SHELL autostart desktop",
        "ZOO_OS PROC start pid=5 name=zookeeper",
        "ZOO_OS PROC start pid=6 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=6 name=peacock-handler",
        "ZOO_OS PROC start pid=7 name=files",
        "ZOO_OS PROC start pid=8 name=crash-test",
        "ZOO_OS PROC start pid=9 name=assistant",
        "ZOO_OS PROC exit pid=6 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=6",
    ]);
    let fast_boot_desktop_exits = |m: Mode| {
        let mut e = expect(m, &fast_head);
        e.then_any_order(&[
            "ZOO_OS PROC exit pid=7 name=files code=0",
            "ZOO_OS PROC exit pid=8 name=crash-test code=0",
            "ZOO_OS PROC exit pid=9 name=assistant code=0",
            "ZOO_OS PROC exit pid=5 name=zookeeper code=0",
        ]);
        e
    };
    let mut fast_boot = fast_boot_desktop_exits(Mode::FastBoot);
    fast_boot.then(&[
        "ZOO_OS SHELL line=run tern-test fast",
        "ZOO_OS PROC start pid=10 name=tern-test",
        "ZOO_OS PROC exit pid=10 name=tern-test code=0",
    ]);
    let mut keyboard = normal.clone();
    keyboard.then(&["ZOO_OS SHELL line=echo hi", "ZOO_OS SHELL line=mem"]);
    let mut scroll = normal.clone();
    scroll
        .then(&["ZOO_OS SHELL line=help"; 4])
        .then(&["ZOO_OS SHELL line=echo end"]);
    let mut never = normal.clone();
    never.then(&["ZOO_OS NEVER-EMITTED"]);
    let desktop = desktop_expectation(&normal);
    let mouse = mouse_expectation(&normal);
    let redraw = mouse_expectation(&normal);
    let frames = mouse_expectation(&normal);
    let frames_gpu = frames.clone();
    let dock = dock_expectation(&normal);
    let notes = notes_expectation(&normal);
    let mut menu_clock = normal.clone();
    menu_clock.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=assistant",
    ]);
    // The tablet case closes Crash Test.
    let mut tablet = menu_clock.clone();
    tablet.then(&["ZOO_OS PROC exit pid=29 name=crash-test code=0"]);
    let mut args_clock = normal.clone();
    args_clock.then(&[
        "ZOO_OS SHELL line=run tern-test alpha beta",
        "ZOO_OS PROC start pid=26 name=tern-test",
        "ZOO_OS PROC exit pid=26 name=tern-test code=0",
    ]);
    let mut inference = normal.clone();
    inference.then(&[
        "ZOO_OS SHELL line=run elephant-handler",
        "ZOO_OS PROC start pid=26 name=elephant-handler",
        "ZOO_OS PROC exit pid=26 name=elephant-handler code=0",
    ]);
    let model_image = match crate::infer::load_model(root) {
        Ok(data) => {
            let r = crate::infer::run(
                &data,
                crate::infer::GUEST_PROMPT,
                crate::infer::GUEST_TOKENS,
            )?;
            println!(
                "[verify] host engine: `{}` -> {:?} ({})",
                crate::infer::GUEST_PROMPT,
                r.text,
                crate::infer::ids(&r.output_ids)
            );
            Some(image::build_model_image(
                root,
                tools,
                &data,
                crate::infer::GUEST_PROMPT,
                &r.output_ids,
            )?)
        }
        Err(e) if s.skip_model => {
            println!(
                "[verify] model unavailable ({e}); the inference case will NOT run (--skip-model)"
            );
            None
        }
        Err(e) => {
            return Err(format!(
                "{e}\n(use --skip-model to run verify without the inference case)"
            ));
        }
    };

    // Wine (docs/linux10-design.md), when tools/wine/export.sh has run.
    let wine_image = image::build_wine_image(root, tools)?;
    let mut wine_version = fast_boot_desktop_exits(Mode::FastBoot);
    wine_version.then(&[
        "ZOO_OS SHELL line=linux /usr/bin/wine --version",
        "ZOO_OS PROC start pid=10 name=wine",
        "ZOO_OS PROC exit pid=10 name=wine code=0",
    ]);

    let mut wine_cmd = fast_boot_desktop_exits(Mode::FastBoot);
    wine_cmd.then(&[
        "ZOO_OS SHELL line=linux WINEPREFIX=/tmp/wine /usr/bin/wine cmd /c echo %OS%",
        "ZOO_OS PROC start pid=10 name=wine",
    ]);
    // Then Wine's own processes, in no fixed order (docs/linux10b-design.md).
    wine_cmd.then_rest();

    // Wine with its default prefix, /home/user/.wine, on a disk kept for a
    // second boot (docs/linux10d-design.md).
    let wine_home = |first: bool| {
        let mut e = fast_boot_desktop_exits(Mode::FastBoot);
        e.then(&[
            "ZOO_OS SHELL line=linux /usr/bin/wine cmd /c echo %OS%",
            "ZOO_OS PROC start pid=10 name=wine",
        ]);
        e.then_rest();
        let mut steps = vec![
            Action::WaitFor("ZOO_OS PROC start pid=9 name=assistant".into()),
            Action::Settle(QUIET),
            Action::Keys(vec!["esc"]),
            Action::WaitFor("ZOO_OS DISPLAY released pid=6".into()),
            Action::Settle(QUIET),
            Action::Keys(typed("linux /usr/bin/wine cmd /c echo %OS%")),
            Action::WaitContains(":wine-preloader] Windows_NT".into()),
            Action::WaitContains("PROC exit pid=10 name=wine-preloader code=0".into()),
        ];
        // wineserver exits a few seconds after its last client, once
        // Wine's start-up work is done; then BadgerHandler saves.
        steps.push(Action::WaitContains("name=wineserver code=0".into()));
        if first {
            steps.push(Action::WaitContains(
                "BadgerHandler: saved generation".into(),
            ));
            steps.push(Action::Settle(Duration::from_secs(3)));
        }
        Case {
            name: if first { "wine-home-1" } else { "wine-home-2" },
            mode: Mode::FastBoot,
            custom_image: wine_image.clone(),
            disk: Disk::Shared {
                name: "wine-home",
                reset: first,
                corrupt: false,
            },
            usb: Usb::None,
            expectation: e,
            timeout: s.timeout * 16,
            want: "pass",
            purpose: if first {
                "Wine creates its prefix in the home folder (ZooFS 2) and runs cmd.exe"
            } else {
                "after a reboot Wine finds its prefix in the home folder and runs cmd.exe"
            },
            interaction: Some(Interaction { steps }),
            screen_checks: vec![],
        }
    };

    let cases = [
        wine_home(true),
        wine_home(false),
        Case {
            name: "wine-cmd",
            mode: Mode::FastBoot,
            custom_image: wine_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: wine_cmd,
            // 176-260 s under TCG: Wine creates its prefix on first run.
            timeout: s.timeout * 16,
            want: "pass",
            purpose: "Wine runs cmd.exe, a Windows program, with its prefix in /tmp",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS PROC start pid=9 name=assistant".into()),
                    Action::Settle(QUIET),
                    Action::Keys(vec!["esc"]),
                    Action::WaitFor("ZOO_OS DISPLAY released pid=6".into()),
                    Action::Settle(QUIET),
                    Action::Keys(typed("linux WINEPREFIX=/tmp/wine /usr/bin/wine cmd /c echo %OS%")),
                    // cmd.exe's own output line, not the shell's echo of
                    // the command (which has `%OS%`).
                    Action::WaitContains(":wine-preloader] Windows_NT".into()),
                    Action::WaitContains("PROC exit pid=10 name=wine-preloader code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "wine-version",
            mode: Mode::FastBoot,
            custom_image: wine_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: wine_version,
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "Wine 11.0 (built for musl) starts under musl's loader and prints its version",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS PROC start pid=9 name=assistant".into()),
                    Action::Settle(QUIET),
                    Action::Keys(vec!["esc"]),
                    Action::WaitFor("ZOO_OS DISPLAY released pid=6".into()),
                    Action::Settle(QUIET),
                    Action::Keys(typed("linux /usr/bin/wine --version")),
                    Action::WaitContains("wine-11.0".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=10 name=wine code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "normal",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: normal.clone(),
            timeout: s.timeout,
            want: "pass",
            purpose: "self-tests, the Zookeeper process scenario, READY; no PANIC or EXCEPTION",
            interaction: None,
            screen_checks: vec![],
        },
        Case {
            name: "fast-boot",
            mode: Mode::FastBoot,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: fast_boot,
            timeout: s.timeout,
            want: "pass",
            purpose: "fast boot: no check programs; starts in the desktop; after Escape the shell runs a C program",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS PROC start pid=9 name=assistant".into()),
                    Action::Settle(QUIET),
                    Action::Screenshot("1-desktop"),
                    Action::Keys(vec!["esc"]),
                    Action::WaitFor("ZOO_OS DISPLAY released pid=6".into()),
                    Action::Settle(QUIET),
                    Action::Keys(typed("run tern-test fast")),
                    Action::WaitContains("TernTest: argc=2 argv=tern-test,fast".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=10 name=tern-test code=0".into()),
                ],
            }),
            screen_checks: vec![("1-desktop", check_desktop_started)],
        },
        Case {
            name: "panic",
            mode: Mode::Panic,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: expect(Mode::Panic, &with_tests("ZOO_OS PANIC intentional-test")),
            timeout: s.timeout,
            want: "pass",
            purpose: "self-tests, then PANIC with interrupts on; no READY",
            interaction: None,
            screen_checks: vec![],
        },
        Case {
            name: "fault-write-protect",
            mode: Mode::FaultWriteProtect,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: expect(
                Mode::FaultWriteProtect,
                &[
                    "ZOO_OS EXCEPTION page-fault access=write cause=protection addr=0xffffc00040000000",
                ],
            ),
            timeout: s.timeout,
            want: "pass",
            purpose: "write to a read-only page is reported as a protection fault",
            interaction: None,
            screen_checks: vec![],
        },
        Case {
            name: "fault-heap-guard",
            mode: Mode::FaultHeapGuard,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: expect(
                Mode::FaultHeapGuard,
                &[
                    // The page after the 8 MiB heap (docs/linux10b-design.md).
                    "ZOO_OS EXCEPTION page-fault access=read cause=not-present addr=0xffffc00000801000",
                ],
            ),
            timeout: s.timeout,
            want: "pass",
            purpose: "read of the heap guard page is reported as not-present",
            interaction: None,
            screen_checks: vec![],
        },
        Case {
            name: "fault-stack-overflow",
            mode: Mode::FaultStackOverflow,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: expect(
                Mode::FaultStackOverflow,
                &["ZOO_OS EXCEPTION double-fault stack-guard-hit"],
            ),
            timeout: s.timeout,
            want: "pass",
            purpose: "stack overflow hits the guard page and is reported via the IST stack",
            interaction: None,
            screen_checks: vec![],
        },
        Case {
            name: "keyboard",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: keyboard,
            timeout: s.timeout,
            want: "pass",
            purpose: "keys typed via QMP reach the shell (with Backspace); screen shows the text",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    // "echx", Backspace, "o hi", Enter, then "mem", Enter.
                    Action::Keys(vec![
                        "e",
                        "c",
                        "h",
                        "x",
                        "backspace",
                        "o",
                        "spc",
                        "h",
                        "i",
                        "ret",
                        "m",
                        "e",
                        "m",
                        "ret",
                    ]),
                    Action::WaitFor("ZOO_OS SHELL line=mem".into()),
                    Action::Settle(QUIET),
                    Action::Screenshot("screen"),
                ],
            }),
            screen_checks: vec![("screen", check_echo)],
        },
        Case {
            name: "scroll",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: scroll,
            timeout: s.timeout,
            want: "pass",
            purpose: "four `help` outputs overflow the screen; the console scrolls correctly",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(scroll_keys()),
                    Action::WaitFor("ZOO_OS SHELL line=echo end".into()),
                    Action::Settle(QUIET),
                    Action::Screenshot("screen"),
                ],
            }),
            screen_checks: vec![("screen", check_scrolled)],
        },
        Case {
            name: "desktop",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop.clone(),
            // Boot, five interaction phases, two restarts: allow longer.
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "desktop: keyboard navigation, file browsing, app and window-server crash recovery, clean exit",
            interaction: Some(desktop_steps()),
            screen_checks: vec![
                ("1-desktop", check_desktop_started),
                ("2-details", check_file_details),
                ("3-app-restart", check_app_restart),
                ("4-server-restart", check_server_restart),
                ("5-console", check_console_back),
            ],
        },
        Case {
            name: "mouse",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse,
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "desktop mouse: pointer drawn, a click focuses a window, a title-bar drag moves it",
            interaction: Some(mouse_steps()),
            screen_checks: vec![
                ("1-pointer", check_pointer),
                ("2-click", check_click_focus),
                ("3-drag", check_drag),
            ],
        },
        Case {
            name: "redraw",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: redraw.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "partial redraws: after pointer moves, a click and a drag, the screen equals a full repaint",
            interaction: Some(redraw_steps()),
            screen_checks: vec![("1-partial", check_partial), ("2-full", check_full_repaint)],
        },
        Case {
            name: "redraw-content",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: redraw.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "content updates redrawn in place in a window's layer: after Files' selection moves, the screen equals a full repaint from scratch",
            interaction: Some(redraw_content_steps()),
            screen_checks: vec![("1-content", check_content_moved), ("2-full", check_full_repaint)],
        },
        Case {
            name: "windows",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: windows_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "several windows per application: Ctrl+N opens a second Files window, cascaded and focused; closing it leaves the first",
            interaction: Some(windows_steps()),
            screen_checks: vec![("1-two", check_two_files), ("2-one", check_one_files)],
        },
        Case {
            name: "dock-menu",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: dock_menu_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "right-click menus on dock icons: New Window opens another Files window; Remove from Dock takes Notes out of the dock",
            interaction: Some(dock_menu_steps()),
            screen_checks: vec![("1-menu", check_dock_menu), ("2-removed", check_notes_removed)],
        },
        Case {
            name: "apps-folder",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: apps_folder_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "Files' Applications folder: Add to Dock puts Notes back after Remove from Dock; the dock's pins are saved and load again after a desktop restart",
            interaction: Some(apps_folder_steps()),
            screen_checks: vec![
                ("1-folder", check_apps_folder),
                ("2-pinned", check_notes_back),
                ("3-reloaded", check_notes_removed),
            ],
        },
        Case {
            name: "dock-minis",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "minimised windows sit after a divider in the dock, one thumbnail each; a thumbnail's click restores that window",
            interaction: Some(dock_minis_steps()),
            screen_checks: vec![("1-minis", check_dock_minis), ("2-back", check_files_back)],
        },
        Case {
            name: "logo",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse_expectation(&normal),
            timeout: s.timeout,
            want: "pass",
            purpose: "the ZooOS mark in the menu bar is the soft paw: a filled main pad, a gap between the two upper toes",
            interaction: Some(Interaction {
                steps: {
                    let mut steps = start_desktop();
                    steps.extend([
                        Action::Screenshot("1-logo"),
                        Action::Keys(vec!["esc"]),
                        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
                    ]);
                    steps
                },
            }),
            screen_checks: vec![("1-logo", check_logo)],
        },
        Case {
            name: "dock-autohide",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "the dock hides itself: hidden at start, slides in at the bottom edge, slides out after the pointer leaves",
            interaction: Some(dock_autohide_steps()),
            screen_checks: vec![
                ("1-hidden", check_dock_hidden),
                ("2-shown", check_dock_revealed),
                ("3-hidden-again", check_dock_hidden),
            ],
        },
        Case {
            name: "animations",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: dock.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "window animations: close, minimize, restore and open each run for their length over several frames, then the desktop is as without them",
            interaction: Some(animation_steps()),
            screen_checks: vec![("1-after", check_after_animations)],
        },
        Case {
            name: "genie",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "the genie: a minimise in slow motion (Shift held), caught early: the window's top still in place at full width while its bottom has pinched toward the dock",
            interaction: Some(genie_steps()),
            screen_checks: vec![("1-genie", check_genie)],
        },
        Case {
            name: "frames",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: frames,
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "frame cost of pointer movement, full repaints vs partial redraws (numbers meaningful with --accel)",
            interaction: Some(frames_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "args-clock",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: args_clock,
            timeout: s.timeout,
            want: "pass",
            purpose: "a C program reads its arguments and the clock: argv, wall time near the host's, monotonic time",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run tern-test alpha beta")),
                    Action::WaitContains("TernTest: argc=3 argv=tern-test,alpha,beta".into()),
                    Action::WaitClock("TernTest: unix=".into()),
                    Action::WaitContains("TernTest: monotonic +".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=tern-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "usb",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::Direct,
            expectation: {
                let mut e = normal.clone();
                e.then(&["ZOO_OS SHELL line=echo usb"]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "USB: the xHCI controller enumerates a keyboard and a mouse; the shell is typed into through the USB keyboard",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitContains("ZOO_OS INFO usb xhci ports=".into()),
                    Action::WaitContains("hid=keyboard".into()),
                    Action::WaitContains("input ready".into()),
                    Action::WaitContains("hid=mouse".into()),
                    Action::WaitContains("input ready".into()),
                    Action::WaitContains("ZOO_OS INFO usb devices=2".into()),
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("echo usb")),
                    Action::WaitContains("ZOO_OS INFO usb keyboard input".into()),
                    Action::WaitFor("ZOO_OS SHELL line=echo usb".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "usb-repeat",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::Direct,
            expectation: {
                // At least ten x's: one press and repeats (about 34
                // expected); the exact count depends on timing.
                let mut e = normal.clone();
                e.then_prefix("ZOO_OS SHELL line=xxxxxxxxxx");
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "USB keyboard repeat: holding x for 1.5 s types it many times, then it stops",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::HoldKey("x", 1500),
                    // The serial log goes quiet only if the repeat stopped
                    // when the key was released.
                    Action::Settle(QUIET),
                    Action::Keys(vec!["ret"]),
                    Action::WaitContains("ZOO_OS SHELL line=xxxxxxxxxx".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "usb-hub",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::Hub,
            expectation: {
                let mut e = normal.clone();
                e.then(&["ZOO_OS SHELL line=echo hub"]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "USB hub: the keyboard and mouse behind a hub are found and the shell is typed into through the keyboard",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitContains("class=9".into()),
                    Action::WaitContains("hid=keyboard".into()),
                    Action::WaitContains(".1 input ready".into()),
                    Action::WaitContains("hid=mouse".into()),
                    Action::WaitContains(".2 input ready".into()),
                    Action::WaitContains("ZOO_OS INFO usb devices=3".into()),
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("echo hub")),
                    Action::WaitContains("ZOO_OS INFO usb keyboard input".into()),
                    Action::WaitFor("ZOO_OS SHELL line=echo hub".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "usb-desktop",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::Direct,
            expectation: menu_clock.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "USB: the desktop is used through the USB mouse (a click focuses a window)",
            interaction: Some(Interaction {
                steps: {
                    let mut steps = start_desktop();
                    steps.extend([
                        // From the centre into Crash Test, click.
                        Ptr::start().to(inside(CRASH_WIN)),
                        Action::Button(true),
                        Action::Button(false),
                        Action::WaitContains("ZOO_OS INFO usb mouse input".into()),
                        Action::WaitContains("PeacockHandler: click focuses Crash Test".into()),
                        Action::Settle(QUIET),
                        Action::Screenshot("1-click"),
                    ]);
                    // USB reports reach the window server when the
                    // controller's interrupt arrives, not at the next tick
                    // (docs/u1c-design.md).
                    steps.push(Action::WaitSeen("ZOO_OS INFO usb interrupts=msix".into()));
                    steps.push(Action::Keys(vec!["f11"]));
                    steps.push(Action::WaitContains("PeacockHandler: frames".into()));
                    pointer_sweep(&mut steps);
                    steps.push(Action::Settle(QUIET));
                    steps.push(Action::Keys(vec!["f11"]));
                    input_wait_checks(&mut steps);
                    steps
                },
            }),
            screen_checks: vec![("1-click", check_click_focus)],
        },
        Case {
            name: "tablet",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::Tablet,
            expectation: tablet.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "USB tablet (what `run` uses): absolute positions land exactly (a click on a 12 px close light), events arrive by interrupt",
            interaction: Some(tablet_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "gpu-desktop",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop.clone(),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "the desktop case on virtio-vga: the window server takes the GPU, crashes and takes it again, then the console returns in VGA mode",
            interaction: Some(desktop_steps()),
            screen_checks: vec![
                ("1-desktop", check_desktop_started),
                ("2-details", check_file_details),
                ("3-app-restart", check_app_restart),
                ("4-server-restart", check_server_restart),
                ("5-console", check_console_back),
            ],
        },
        Case {
            name: "gpu-frames",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: frames_gpu.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "frame cost on the virtio GPU, flushes included: full repaints on every pointer move (numbers meaningful with --accel)",
            interaction: Some(gpu_frames_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "gpu-fast-drag",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::Tablet,
            expectation: menu_clock.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "a fast drag: 400 tablet moves back to back while dragging Files; events must not queue up behind the frames",
            interaction: Some(fast_drag_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "gpu-hidpi-frames",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: frames_gpu.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "frame cost at 2x on a 3840x2160 screen: full repaints, a window drag, content updates (numbers meaningful with --accel)",
            interaction: Some(gpu_frames_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "gpu-drag-cursor",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::Tablet,
            expectation: menu_clock.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "while a window is dragged the pointer is drawn in the frame with it (the host cursor hidden); after the drag the hardware cursor is back",
            interaction: Some(drag_cursor_steps()),
            screen_checks: vec![
                ("1-dragging", check_drag_pointer_drawn),
                ("2-released", check_drag_pointer_gone),
            ],
        },
        Case {
            name: "gpu-1440",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::Tablet,
            expectation: tablet.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "the owner's monitor: a 2560x1440 host size through fw_cfg, the desktop at 1x over all of it, an exact click",
            interaction: Some(q1440_steps()),
            screen_checks: vec![("1-1440", check_1440)],
        },
        Case {
            name: "gpu-hidpi",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::Tablet,
            expectation: tablet.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "a 3840x2160 screen (the host size through fw_cfg) drawn at 2x: 26 px text where 13 px was, clicks exact in logical pixels",
            interaction: Some(hidpi_steps()),
            screen_checks: vec![("1-hidpi", check_hidpi)],
        },
        Case {
            name: "gpu-tablet",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::Tablet,
            expectation: tablet.clone(),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "what `run` uses: virtio-vga and a tablet; flushed updates, a hardware cursor that moves without redraws, exact clicks",
            interaction: Some(tablet_steps_with(true)),
            screen_checks: vec![("1-gpu-desktop", check_gpu_desktop)],
        },
        Case {
            name: "big-screen",
            mode: Mode::BigScreen,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: big_screen,
            timeout: s.timeout,
            want: "pass",
            purpose: "a 2560x1600 screen: the desktop uses all of it at 1x (docs/u2b-design.md)",
            interaction: Some(Interaction {
                steps: {
                    let mut steps = start_desktop();
                    steps.insert(2, Action::WaitContains("PeacockHandler: display ".into()));
                    steps.push(Action::Screenshot("1-desktop"));
                    steps
                },
            }),
            screen_checks: vec![("1-desktop", check_big_screen)],
        },
        Case {
            name: "linux-hello",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux hello alpha",
                    "ZOO_OS PROC start pid=26 name=hello",
                    "ZOO_OS PROC exit pid=26 name=hello code=0",
                ]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "an unmodified static Linux musl program runs through Linux system calls",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux hello alpha")),
                    Action::WaitFor(
                        "[26:hello] Hello from Linux musl: argc=2 argv[0]=hello argv[1]=alpha"
                            .into(),
                    ),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=hello code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "linux-eider",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux eider-test",
                    "ZOO_OS PROC start pid=26 name=eider-test",
                    "ZOO_OS PROC exit pid=26 name=eider-test code=0",
                ]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "a Linux musl program uses malloc, files (stdio and raw calls) and the clock",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux eider-test")),
                    Action::WaitContains("EiderTest: memory ok".into()),
                    Action::WaitContains("EiderTest: /etc/services 180 bytes, 5 lines".into()),
                    Action::WaitContains("EiderTest: slept ".into()),
                    Action::WaitClock("EiderTest: unix=".into()),
                    Action::WaitContains("EiderTest: all checks passed".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=eider-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "linux-skua",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux skua-test",
                    "ZOO_OS PROC start pid=26 name=skua-test",
                    "ZOO_OS PROC exit pid=26 name=skua-test code=0",
                ]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "a Linux musl program uses uname, its environment, folders, the working directory and access",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux skua-test")),
                    Action::WaitContains("SkuaTest: uname Linux x86_64".into()),
                    Action::WaitContains("SkuaTest: HOME=/home/user USER=user".into()),
                    Action::WaitContains("SkuaTest: /home/user: Documents Music Photos".into()),
                    Action::WaitContains(
                        "SkuaTest: cwd /home/user; Documents/notes.txt starts \"Shopping: flour, eggs, milk.\""
                            .into(),
                    ),
                    Action::WaitContains("SkuaTest: all checks passed".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=skua-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "file-speed",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run bowhead-test",
                    "ZOO_OS PROC start pid=26 name=bowhead-test",
                    "ZOO_OS PROC start pid=27 name=badger-handler",
                    "ZOO_OS PROC killed pid=27 name=badger-handler by=26",
                    "ZOO_OS PROC exit pid=26 name=bowhead-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "file throughput: 1 MiB written and read back through BadgerHandler, timed (numbers meaningful with --accel)",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run bowhead-test")),
                    Action::WaitContains("BowheadTest: 1 MiB write ".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=bowhead-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "linux-fulmar",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux fulmar-test",
                    "ZOO_OS PROC start pid=26 name=fulmar-test",
                    "ZOO_OS PROC exit pid=26 name=fulmar-test code=0",
                    "ZOO_OS SHELL line=cat Fulmar/notes.txt",
                ]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "a Linux musl program writes in the home folder; ZooOS's shell then reads what it wrote",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux fulmar-test")),
                    Action::WaitContains("FulmarTest: /home/user/Fulmar: big.bin notes.txt".into()),
                    Action::WaitContains("FulmarTest: all checks passed".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=fulmar-test code=0".into()),
                    Action::Settle(QUIET),
                    // The same file, through the supervisor and BadgerHandler.
                    Action::Keys(typed("cat Fulmar/notes.txt")),
                    Action::WaitFor("line one".into()),
                    Action::WaitFor("line two".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "linux-guillemot",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux guillemot-test",
                    "ZOO_OS PROC start pid=26 name=guillemot-test",
                ]);
                e.then(&guillemot_children()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>());
                e.then(&["ZOO_OS PROC exit pid=26 name=guillemot-test code=0"]);
                e
            },
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "a Linux musl program forks, pipes, execs, spawns, kills and waits",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux guillemot-test")),
                    Action::WaitContains(
                        "GuillemotTest: exec output: Hello from Linux musl: argc=2 argv[0]=hello argv[1]=from-guillemot"
                            .into(),
                    ),
                    Action::WaitContains("GuillemotTest: all checks passed".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=guillemot-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "linux-kittiwake",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux kittiwake-test",
                    "ZOO_OS PROC start pid=26 name=kittiwake-test",
                    // EINTR / SA_RESTART sender; threaded child (SIGTERM).
                    "ZOO_OS PROC start pid=27 name=kittiwake-test",
                    "ZOO_OS PROC exit pid=27 name=kittiwake-test code=0",
                    "ZOO_OS PROC start pid=28 name=kittiwake-test",
                    "ZOO_OS PROC exit pid=28 name=kittiwake-test signal=15",
                    // `ud2` without a handler.
                    "ZOO_OS PROC start pid=29 name=kittiwake-test",
                ]);
                e.then_prefix("ZOO_OS PROC crash pid=29 name=kittiwake-test fault=");
                e.then(&[
                    "ZOO_OS PROC exit pid=29 name=kittiwake-test signal=4",
                    // pause() and a handler.
                    "ZOO_OS PROC start pid=30 name=kittiwake-test",
                    "ZOO_OS PROC exit pid=30 name=kittiwake-test code=0",
                    "ZOO_OS PROC exit pid=26 name=kittiwake-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "a Linux musl program uses signal handlers, signal frames and pthreads",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux kittiwake-test")),
                    Action::WaitContains("KittiwakeTest: all checks passed".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=kittiwake-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "linux-ermine",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux ermine-test",
                    "ZOO_OS PROC start pid=26 name=ermine-test",
                    // The socket client; the echo child.
                    "ZOO_OS PROC start pid=27 name=ermine-test",
                    "ZOO_OS PROC exit pid=27 name=ermine-test code=0",
                    "ZOO_OS PROC start pid=28 name=ermine-test",
                    "ZOO_OS PROC exit pid=28 name=ermine-test code=0",
                    "ZOO_OS PROC exit pid=26 name=ermine-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "a Linux musl program uses Unix sockets, descriptor passing, poll, select, epoll and eventfd",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux ermine-test")),
                    Action::WaitContains("ErmineTest: all checks passed".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=ermine-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "linux-loon",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux loon-test",
                    "ZOO_OS PROC start pid=26 name=loon-test",
                    "ZOO_OS SHELL line=ps",
                ]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "the shell still answers while a Linux program spins without ever blocking",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux loon-test")),
                    Action::WaitContains("LoonTest: spinning".into()),
                    Action::Settle(QUIET),
                    Action::Keys(typed("ps")),
                    Action::WaitFor("ZOO_OS SHELL line=ps".into()),
                    Action::WaitContains("loon-test        ready".into()),
                ],
            }),
            screen_checks: vec![],
        },
        petrel_case(&normal, true, s.timeout * 2),
        petrel_case(&normal, false, s.timeout * 2),
        murre_case(&normal, "linux-murre-1", s.timeout * 4),
        murre_case(&normal, "linux-murre-2", s.timeout * 4),
        Case {
            name: "linux-auklet",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux AUKLET=wing TERM=vt100 auklet-test",
                    "ZOO_OS PROC start pid=26 name=auklet-test",
                    // The child sharing a tmpfs mapping.
                    "ZOO_OS PROC start pid=27 name=auklet-test",
                    "ZOO_OS PROC exit pid=27 name=auklet-test code=0",
                    "ZOO_OS PROC exit pid=26 name=auklet-test code=0",
                ]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "a Linux musl program uses /tmp: folders, symbolic links, *at calls, unlinked open files, memfd and MAP_SHARED across fork",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux AUKLET=wing TERM=vt100 auklet-test")),
                    Action::WaitContains("AukletTest: /tmp/auklet: a link".into()),
                    Action::WaitContains("AukletTest: all checks passed".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=auklet-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "linux-dovekie",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux dovekie-test",
                    "ZOO_OS PROC start pid=26 name=dovekie-test",
                ]);
                e.then(&dovekie_children()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>());
                e.then(&["ZOO_OS PROC exit pid=26 name=dovekie-test code=0"]);
                e
            },
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "a dynamically linked Linux program runs under musl's loader, with a shared library and dlopen",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux dovekie-test")),
                    Action::WaitContains("DovekieTest: libdovekie says: hello from libdovekie".into()),
                    Action::WaitContains("DovekieTest: all checks passed".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=dovekie-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "linux-redpoll",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=linux redpoll-test",
                    "ZOO_OS PROC start pid=26 name=redpoll-test",
                    "ZOO_OS PROC start pid=27 name=redpoll-test",
                    "ZOO_OS PROC exit pid=27 name=redpoll-test code=0",
                    "ZOO_OS PROC exit pid=26 name=redpoll-test code=0",
                ]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "a Linux program linked at Wine's preloader address reserves the low 2 GiB, maps at 0x400000 and carves pages",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("linux redpoll-test")),
                    Action::WaitContains("RedpollTest: all checks passed".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=redpoll-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "timer-rate",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run tern-test wait",
                    "ZOO_OS PROC start pid=26 name=tern-test",
                    "ZOO_OS PROC exit pid=26 name=tern-test code=0",
                ]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "the tick runs at 100 Hz: 500 ticks of guest sleep take 5 s of host time (within 20%)",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run tern-test wait")),
                    Action::WaitContains("TernTest: waiting 500 ticks".into()),
                    Action::WaitGap("TernTest: waited".into(), 4000, 6000),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=tern-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "files-browse",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: menu_clock.clone(),
            timeout: s.timeout,
            want: "pass",
            purpose: "Files browses the home folder: open folders, preview a text file, go back up",
            interaction: Some(Interaction {
                steps: {
                    let mut steps = start_desktop();
                    steps.extend([
                        // Documents, then notes.txt.
                        Action::Keys(vec!["ret"]),
                        Action::Settle(QUIET),
                        Action::Keys(vec!["ret"]),
                        Action::Settle(QUIET),
                        Action::Screenshot("1-preview"),
                        // Back to Home; into Photos; back up.
                        Action::Keys(vec!["backspace", "backspace"]),
                        Action::Settle(QUIET),
                        Action::Keys(vec!["down", "down", "ret"]),
                        Action::Settle(QUIET),
                        Action::Keys(vec!["backspace"]),
                        Action::Settle(QUIET),
                        Action::Screenshot("2-back"),
                    ]);
                    steps
                },
            }),
            screen_checks: vec![("1-preview", check_preview), ("2-back", check_back_up)],
        },
        Case {
            name: "menu-clock",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: menu_clock,
            timeout: s.timeout,
            want: "pass",
            purpose: "the menu bar shows the UTC date and time of the moment of the screenshot",
            interaction: Some(Interaction {
                steps: {
                    let mut steps = start_desktop();
                    steps.push(Action::Screenshot("1-menu"));
                    steps
                },
            }),
            screen_checks: vec![("1-menu", check_menu_clock)],
        },
        notes_case(notes.clone(), "notes-1", s.timeout * 2),
        notes_case(notes, "notes-2", s.timeout * 2),
        Case {
            name: "notes-cursor",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: notes_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "Notes edits at a cursor moved with the arrow keys",
            interaction: Some(Interaction {
                steps: {
                    let mut steps = start_desktop();
                    let mut p = Ptr::start();
                    steps.extend([
                        // The Notes icon, then up into its window.
                        // The dock hides itself: reveal it at the bottom edge first.
                        p.to((dock_centre(3).0, SCREEN.1 - 1)),
                        Action::Settle(DOCK_SLIDE),
                        p.to(dock_centre(3)),
                        Action::Button(true),
                        Action::Button(false),
                        Action::WaitContains("PeacockHandler: opened Notes".into()),
                        p.to(inside(3)),
                        // abc, two left, X and Y, Backspace: aX|bc.
                        Action::Keys(vec![
                            "a",
                            "b",
                            "c",
                            "left",
                            "left",
                            "shift+x",
                            "shift+y",
                            "backspace",
                        ]),
                        // 66 seed bytes + 4.
                        Action::WaitContains("Notes: saved 70 bytes".into()),
                        Action::Settle(QUIET),
                        Action::Screenshot("1-cursor"),
                        Action::Keys(vec!["esc"]),
                        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
                    ]);
                    steps
                },
            }),
            screen_checks: vec![("1-cursor", check_notes_cursor)],
        },
        Case {
            name: "appearance",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "the dark appearance (F8), and back to light with the menu bar's switch",
            interaction: Some(appearance_steps()),
            screen_checks: vec![("1-dark", check_dark), ("2-light", check_light_again)],
        },
        Case {
            name: "resize",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "resize a window by dragging its bottom-right corner",
            interaction: Some(Interaction {
                steps: {
                    let mut steps = start_desktop();
                    let c = win(CRASH_WIN);
                    let mut p = Ptr::start();
                    steps.extend([
                        // Crash Test's bottom-right corner, dragged by
                        // (-200, -100).
                        p.to((c.0 + c.2 - 4, c.1 + c.3 - 4)),
                        Action::Button(true),
                        p.by(-200, -100),
                        Action::Button(false),
                        Action::WaitContains(format!(
                            "PeacockHandler: resized Crash Test to {}x{}",
                            c.2 - 200,
                            c.3 - 100
                        )),
                        Action::Settle(QUIET),
                        Action::Screenshot("1-resized"),
                        Action::Keys(vec!["esc"]),
                        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
                    ]);
                    steps
                },
            }),
            screen_checks: vec![("1-resized", check_resized)],
        },
        Case {
            name: "zoom",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "double-click a title bar to zoom a window, and again to restore it",
            interaction: Some(Interaction {
                steps: {
                    let mut steps = start_desktop();
                    let z = zui::zoomed(SCREEN.0, SCREEN.1);
                    let mut p = Ptr::start();
                    steps.extend([
                        // The Assistant's title bar.
                        p.to(title_bar(ASSISTANT_WIN)),
                        Action::Button(true),
                        Action::Button(false),
                        Action::Button(true),
                        Action::Button(false),
                        Action::WaitContains("PeacockHandler: zoomed Assistant".into()),
                        Action::Settle(QUIET),
                        Action::Screenshot("1-zoomed"),
                        // Its title bar is now at the top.
                        p.to((z.0 + z.2 / 2, z.1 + 10)),
                        Action::Button(true),
                        Action::Button(false),
                        Action::Button(true),
                        Action::Button(false),
                        Action::WaitContains("PeacockHandler: restored Assistant".into()),
                        Action::Settle(QUIET),
                        Action::Screenshot("2-restored"),
                        Action::Keys(vec!["esc"]),
                        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
                    ]);
                    steps
                },
            }),
            screen_checks: vec![("1-zoomed", check_zoomed), ("2-restored", check_restored)],
        },
        Case {
            name: "dock",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: dock,
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "dock and close buttons: close Crash Test (not restarted), reopen it from the dock",
            interaction: Some(dock_steps()),
            screen_checks: vec![
                ("1-dock", check_dock_shown),
                ("2-closed", check_closed),
                ("3-dock-closed", check_dock_closed),
                ("4-dock-reopened", check_dock_reopened),
                ("5-reopened", check_reopened),
            ],
        },
        Case {
            name: "assistant",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: assistant_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "Assistant window: typed commands through OtterHandler, plan + confirm before create, undo, escape refused, model absent reported",
            interaction: Some(assistant_steps()),
            screen_checks: vec![
                ("1-plan", check_assistant_plan),
                ("2-transcript", check_assistant_transcript),
            ],
        },
        Case {
            name: "services",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: services_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "resident supervisor: services listed, crash and hang detected and restarted, restart limit, logs",
            interaction: Some(services_steps()),
            screen_checks: vec![],
        },
        persist_case(&normal, "persist-1", 1, s.timeout * 3),
        persist_case(&normal, "persist-2", 2, s.timeout * 3),
        persist_case(&normal, "persist-3", 3, s.timeout * 3),
        persist_case(&normal, "persist-4", 4, s.timeout * 3),
        persist_case(&normal, "persist-5", 5, s.timeout * 3),
        Case {
            name: "c-program",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run puffin-test",
                    "ZOO_OS PROC start pid=26 name=puffin-test",
                    "ZOO_OS PROC exit pid=26 name=puffin-test code=0",
                ]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "a C program built with the pinned zig toolchain runs from the shell and passes its checks",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run puffin-test")),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=puffin-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "assistant-model",
            mode: Mode::Normal,
            custom_image: model_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: assistant_model_expectation(&normal, false),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "the model works on a request while the desktop stays usable; Backspace cancels it",
            interaction: Some(assistant_model_steps()),
            screen_checks: vec![
                ("1-busy", check_busy_desktop),
                ("2-cancelled", check_cancelled),
            ],
        },
        Case {
            name: "assistant-live",
            mode: Mode::Normal,
            custom_image: model_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: assistant_model_expectation(&normal, true),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "requests in words: the model picks the tool, the broker runs it (needs --accel; too slow under TCG)",
            interaction: Some(assistant_live_steps()),
            screen_checks: vec![("1-live", check_live)],
        },
        Case {
            name: "inference",
            mode: Mode::Normal,
            custom_image: model_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: inference,
            // Software emulation runs the 0.5B model at a fraction of a token
            // per second; allow for loading, tokenizer setup and 33 passes.
            timeout: s.timeout * 60,
            want: "pass",
            purpose: "ElephantHandler runs Qwen2.5-0.5B in the guest; its tokens must equal the host engine's",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(
                        "run elephant-handler"
                            .chars()
                            .map(|c| match c {
                                ' ' => "spc",
                                '-' => "minus",
                                c => KEYS[(c as u8 - b'a') as usize],
                            })
                            .chain(["ret"])
                            .collect(),
                    ),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "negative-wrong-marker",
            mode: Mode::Panic,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: normal.clone(),
            timeout: s.timeout,
            want: "unexpected-marker",
            purpose: "verifier must reject the panic image under normal expectations",
            interaction: None,
            screen_checks: vec![],
        },
        Case {
            name: "negative-timeout",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: never,
            timeout: s.negative_timeout,
            want: "timeout",
            purpose: "verifier must reject a marker that never appears, by timeout",
            interaction: None,
            screen_checks: vec![],
        },
        Case {
            name: "negative-triple-fault",
            mode: Mode::TripleFault,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: expect(Mode::TripleFault, &["ZOO_OS READY"]),
            timeout: s.timeout,
            want: "early-exit",
            purpose: "verifier must reject a guest that triple-faults (reset with -no-reboot)",
            interaction: None,
            screen_checks: vec![],
        },
    ];

    let mut report = String::new();
    let mut all_ok = true;
    if let Some(only) = &s.only
        && !cases.iter().any(|c| c.name == only)
    {
        return Err(format!("no case named `{only}`"));
    }
    for c in &cases {
        if s.only.as_ref().is_some_and(|o| o != c.name) {
            continue;
        }
        let model_case = matches!(c.name, "inference" | "assistant-model" | "assistant-live");
        if model_case && c.custom_image.is_none() {
            let line = format!(
                "SKIP  {:<22} not run: model unavailable (--skip-model)\n",
                c.name
            );
            print!("{line}");
            report.push_str(&line);
            continue;
        }
        if c.name.starts_with("wine-") && c.custom_image.is_none() {
            let line = format!(
                "SKIP  {:<22} not run: no Wine export in build/wine (tools/wine/README.md)
",
                c.name
            );
            print!("{line}");
            report.push_str(&line);
            continue;
        }
        if c.name == "c-program" && crate::cprog::zig(root).is_none() {
            let line = format!(
                "SKIP  {:<22} not run: no C toolchain in tools/zig/ (see tools/zig/README.md)
",
                c.name
            );
            print!("{line}");
            report.push_str(&line);
            continue;
        }
        // The live model, and Wine creating its 167 MB prefix in the home
        // folder (over 8 minutes under TCG, 67-77 s with WHPX; the second
        // boot needs the first's disk), run with an accelerator only.
        if matches!(c.name, "assistant-live" | "wine-home-1" | "wine-home-2")
            && qemu::accel() == "tcg"
        {
            let line = format!(
                "SKIP  {:<22} not run: needs a hardware accelerator (--accel whpx or kvm)\n",
                c.name
            );
            print!("{line}");
            report.push_str(&line);
            continue;
        }
        let img = &images
            .iter()
            .find(|(m, _)| *m == c.mode)
            .expect("every mode was built")
            .1;
        let img = c.custom_image.as_ref().unwrap_or(img);
        println!(
            "[verify] case {} ({}; timeout {:?})",
            c.name, c.purpose, c.timeout
        );
        let data = match c.disk {
            Disk::Fresh => {
                let p = root
                    .join("build")
                    .join("disks")
                    .join(format!("{}.img", c.name));
                qemu::ensure_data_disk(&p, true)?;
                p
            }
            Disk::Shared {
                name,
                reset,
                corrupt,
            } => {
                let p = root.join("build").join("disks").join(format!("{name}.img"));
                qemu::ensure_data_disk(&p, reset)?;
                if corrupt {
                    let what = corrupt_newest_snapshot(&p)?;
                    println!("[verify] {what}");
                }
                p
            }
        };
        qemu::set_usb(c.usb);
        // `gpu-` cases run on `virtio-vga`; the rest on plain VGA, the
        // real-hardware path (docs/gpu1-design.md).
        qemu::set_display(if c.name.starts_with("gpu-") {
            qemu::Display::VirtioVga
        } else {
            qemu::Display::Vga
        });
        // `gpu-hidpi` cases: a 4K screen, drawn at 2x (docs/u2b-design.md).
        qemu::set_display_size(if c.name.starts_with("gpu-hidpi") {
            Some((3840, 2160))
        } else if c.name == "gpu-1440" {
            Some((2560, 1440))
        } else {
            None
        });
        let r = qemu::run_case(
            root,
            tools,
            c.name,
            img,
            &c.expectation,
            qemu::Limits {
                timeout: c.timeout,
                settle: s.settle,
            },
            c.interaction.as_ref(),
            &data,
        )?;
        // A case's own disk is not needed after it (shared disks are kept
        // for the next boot).
        if matches!(c.disk, Disk::Fresh) {
            let _ = std::fs::remove_file(&data);
        }
        let mut kind = r.outcome.kind().to_string();
        let mut detail = r.outcome.detail().to_string();
        if kind == "pass" {
            PREVIOUS.with(|p| p.borrow_mut().take());
            for (name, check) in &c.screen_checks {
                let Some((_, ppm)) = r.screenshots.iter().find(|(n, _)| n == name) else {
                    kind = "harness-error".into();
                    detail = format!("screenshot `{name}` was not taken");
                    break;
                };
                SHOT_TIME.with(|t| t.set(std::fs::metadata(ppm).and_then(|m| m.modified()).ok()));
                let checked = screen::read_ppm(ppm).and_then(|img| {
                    let result = check(&img);
                    PREVIOUS.with(|p| *p.borrow_mut() = Some(img));
                    result
                });
                match checked {
                    Ok(summary) => detail = format!("{detail}\n      screen {name}: {summary}"),
                    Err(why) => {
                        kind = "screen-mismatch".into();
                        detail = format!("screen {name}: {why} ({})", ppm.display());
                        break;
                    }
                }
            }
        }
        let ok = kind == c.want;
        all_ok &= ok;
        let line = format!(
            "{:<5} {:<22} expected={:<17} got={:<17} {:>6.1}s  qemu {}\n      detail: {}\n      log: {}\n",
            if ok { "OK" } else { "FAIL" },
            c.name,
            c.want,
            kind,
            r.elapsed.as_secs_f64(),
            r.shutdown,
            detail,
            r.log.display(),
        );
        print!("{line}");
        report.push_str(&line);
        if let (false, Outcome::Pass) = (ok, &r.outcome) {
            println!("      (a negative case passing means the verifier is not discriminating)");
        }
    }
    let _ = writeln!(
        report,
        "result: {}",
        if all_ok {
            "ALL CASES BEHAVED AS EXPECTED"
        } else {
            "VERIFICATION FAILED"
        }
    );
    let summary = root.join("build").join("runs").join("summary.txt");
    let _ = std::fs::write(&summary, &report);
    println!("[verify] summary written to {}", summary.display());
    if all_ok {
        println!(
            "[verify] PASS: all positive cases passed and all negative cases were rejected as intended"
        );
        Ok(ExitCode::SUCCESS)
    } else {
        println!("[verify] FAIL");
        Ok(ExitCode::FAILURE)
    }
}

fn scroll_keys() -> Vec<&'static str> {
    let mut keys = Vec::new();
    for _ in 0..4 {
        keys.extend(["h", "e", "l", "p", "ret"]);
    }
    keys.extend(["e", "c", "h", "o", "spc", "e", "n", "d", "ret"]);
    keys
}

/// Checks the keyboard case's screenshot against text re-rendered with the
/// kernel's font and layout (docs/console-design.md §4).
fn check_echo(img: &screen::Image) -> Result<String, String> {
    // Since M3 the boot log is longer than one screen, so anchor on lines
    // that stay visible: READY, then the typed command and its output.
    let ready = screen::find_row(img, "ZOO_OS READY").ok_or("no row shows `ZOO_OS READY`")?;
    let row = screen::find_row(img, "zoo> echo hi").ok_or("no row shows `zoo> echo hi`")?;
    if row <= ready {
        return Err(format!(
            "`zoo> echo hi` (row {row}) is not below READY (row {ready})"
        ));
    }
    if !screen::row_matches(img, "hi", row + 1) {
        return Err(format!(
            "row {} below the command does not show `hi`",
            row + 1
        ));
    }
    let last = screen::rows(img) - 1;
    if !screen::row_matches(img, "zoo> ", last) {
        return Err(format!("last row {last} does not show the prompt"));
    }
    // The same comparison must reject a wrong string, or it proves nothing.
    if screen::find_row(img, "zoo> echo ho").is_some() {
        return Err("a wrong string matched; the check is not discriminating".into());
    }
    Ok(format!(
        "{}x{}, {} lit pixels; row {ready} = READY, row {row} = `zoo> echo hi`, row {} = `hi`, row {last} = prompt; wrong text rejected",
        img.width,
        img.height,
        screen::lit_pixels(img),
        row + 1
    ))
}

/// After the screen has scrolled, the newest lines must sit at the bottom:
/// the empty prompt on the last row, `end` above it, the command above that.
/// The BOOT line must have scrolled off row 0.
fn check_scrolled(img: &screen::Image) -> Result<String, String> {
    let rows = screen::rows(img);
    let last = rows.checked_sub(1).ok_or("screen has no text rows")?;
    let want = [
        (last.saturating_sub(2), "zoo> echo end"),
        (last.saturating_sub(1), "end"),
        (last, "zoo> "),
    ];
    for (row, text) in want {
        if !screen::row_matches(img, text, row) {
            return Err(format!("row {row} does not show `{text}`"));
        }
    }
    let boot = format!("ZOO_OS BOOT version={VERSION}");
    if screen::row_matches(img, &boot, 0) {
        return Err("BOOT line is still on row 0; the screen did not scroll".into());
    }
    Ok(format!(
        "{rows} rows; rows {}-{last} = `zoo> echo end`, `end`, prompt; BOOT scrolled off",
        last - 2
    ))
}

// ---- M4 desktop case (docs/m4-design.md §8) ---------------------------

const PAGE_FAULT_KERNEL_READ: &str =
    "fault=page-fault access=read cause=protection addr=0xffffc00000001000";

fn desktop_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    // 1. Start the session: the window server and its three applications.
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=assistant",
    ]);
    // 3. Crash Test crashes; the window server restarts it.
    let crash_test_crash =
        format!("ZOO_OS PROC crash pid=29 name=crash-test {PAGE_FAULT_KERNEL_READ}");
    e.then(&[
        &crash_test_crash,
        "ZOO_OS PROC start pid=31 name=crash-test",
    ]);
    // 4. The window server crashes; its apps end while Zookeeper restarts it.
    let server_crash =
        format!("ZOO_OS PROC crash pid=27 name=peacock-handler {PAGE_FAULT_KERNEL_READ}");
    e.then(&[&server_crash, "ZOO_OS DISPLAY released pid=27"]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=31 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=assistant code=0",
        "ZOO_OS PROC start pid=32 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=32 name=peacock-handler",
        "ZOO_OS PROC start pid=33 name=files",
        "ZOO_OS PROC start pid=34 name=crash-test",
        "ZOO_OS PROC start pid=35 name=assistant",
    ]);
    // 5. Escape ends the session cleanly; the kernel shell is back.
    e.then(&[
        "ZOO_OS PROC exit pid=32 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=32",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=33 name=files code=0",
        "ZOO_OS PROC exit pid=34 name=crash-test code=0",
        "ZOO_OS PROC exit pid=35 name=assistant code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e.then(&["ZOO_OS SHELL line=echo back"]);
    e.clone()
}

/// The desktop with the Assistant used through typed commands; the normal
/// image has no model, so a request in words reaches ElephantHandler, which
/// reports that (docs/m5c2-design.md §5).
fn assistant_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=assistant",
        // Started on first use: the broker by the first command, the model
        // service by the first request in words.
        "ZOO_OS PROC start pid=31 name=otter-handler",
        "ZOO_OS PROC start pid=32 name=elephant-handler",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=assistant code=0",
        "ZOO_OS PROC exit pid=31 name=otter-handler code=0",
        "ZOO_OS PROC exit pid=32 name=elephant-handler code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e.clone()
}

fn assistant_steps() -> Interaction {
    let mut steps = vec![
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(typed("desktop")),
        Action::WaitFor("ZOO_OS PROC start pid=30 name=assistant".into()),
        Action::Settle(QUIET),
        // Files -> Crash Test -> Assistant.
        Action::Keys(vec!["tab", "tab"]),
        Action::Settle(QUIET),
    ];
    for line in ["search pancake", "create Projects/2026"] {
        steps.push(Action::Keys(typed(line)));
        steps.push(Action::Settle(QUIET));
    }
    steps.push(Action::Screenshot("1-plan"));
    steps.push(Action::Keys(vec!["y"]));
    steps.push(Action::Settle(QUIET));
    for line in [
        "undo",
        "inspect Projects",
        "list ../bin",
        "find my pancake recipe",
    ] {
        steps.push(Action::Keys(typed(line)));
        steps.push(Action::Settle(QUIET));
    }
    steps.push(Action::WaitFor(
        "ZOO_OS PROC start pid=32 name=elephant-handler".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("2-transcript"));
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

fn check_assistant_plan(img: &screen::Image) -> Result<String, String> {
    let title = title(img, "Assistant", true)?;
    content(img, "You: search pancake")?;
    // Two leading spaces: the broker's reply, not the Files window's
    // `home/user/Documents/...` entry.
    let found = content(img, "  Documents/recipes/pancakes.txt")?;
    content(img, "  would create Projects")?;
    content(img, "  would create Projects/2026")?;
    let ask = content(img, "Press y to create, n to leave it.")?;
    // Nothing is created before the confirmation.
    no_content(img, "  created Projects")?;
    Ok(format!(
        "`Assistant` focused at {title:?}; search result at {found:?}; plan shown and confirmation asked at {ask:?}; nothing created yet"
    ))
}

fn check_assistant_transcript(img: &screen::Image) -> Result<String, String> {
    let created = content(img, "  created Projects/2026")?;
    content(img, "  removed Projects/2026")?;
    let removed = content(img, "  removed Projects")?;
    content(img, "Error: not found")?;
    let escape = content(img, "Error: path escapes home")?;
    let model = content(img, "The model is not installed; type a command instead.")?;
    no_content(img, "  created Projects/2027")?;
    Ok(format!(
        "created at {created:?}, undone at {removed:?}, then not found; escape refused at {escape:?}; missing model reported at {model:?}; wrong text rejected"
    ))
}

/// Desktop start, the model service (pid 14) and, for the live case, the
/// broker (pid 15); then a clean exit.
fn assistant_model_expectation(normal: &Expectation, live: bool) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=assistant",
        "ZOO_OS PROC start pid=31 name=elephant-handler",
    ]);
    if live {
        e.then(&["ZOO_OS PROC start pid=32 name=otter-handler"]);
    }
    e.then(&[
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    let mut exits = vec![
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=assistant code=0",
        "ZOO_OS PROC exit pid=31 name=elephant-handler code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ];
    if live {
        exits.push("ZOO_OS PROC exit pid=32 name=otter-handler code=0");
    }
    e.then_any_order(&exits);
    e.clone()
}

fn open_assistant() -> Vec<Action> {
    vec![
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(typed("desktop")),
        Action::WaitFor("ZOO_OS PROC start pid=30 name=assistant".into()),
        Action::Settle(QUIET),
        Action::Keys(vec!["tab", "tab"]),
        Action::Settle(QUIET),
    ]
}

fn leave_desktop(steps: &mut Vec<Action>) {
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()));
    steps.push(Action::Settle(QUIET));
}

fn assistant_model_steps() -> Interaction {
    let mut steps = open_assistant();
    steps.extend([
        Action::Keys(typed("find my pancake recipe")),
        Action::WaitFor(
            "[31:elephant-handler] ElephantHandler: service ready (model mapped)".into(),
        ),
        Action::Settle(QUIET),
        // While the model computes: move to Files and down two entries.
        Action::Keys(vec!["tab", "down", "down"]),
        Action::Settle(QUIET),
        Action::Screenshot("1-busy"),
        // Back to the Assistant (Files -> Crash Test -> Assistant) and cancel.
        Action::Keys(vec!["tab", "tab", "backspace"]),
        Action::WaitFor("[31:elephant-handler] ElephantHandler: request cancelled".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-cancelled"),
    ]);
    leave_desktop(&mut steps);
    Interaction { steps }
}

fn assistant_live_steps() -> Interaction {
    let mut steps = open_assistant();
    steps.extend([
        Action::Keys(typed("Find my pancake recipe")),
        Action::WaitFor(
            "[31:elephant-handler] ElephantHandler: chose search `pancake recipe`".into(),
        ),
        Action::Settle(QUIET),
        Action::Keys(typed("Make a folder called Projects/2026")),
        Action::WaitFor(
            "[31:elephant-handler] ElephantHandler: chose create `Projects/2026`".into(),
        ),
        Action::Settle(QUIET),
        Action::Keys(vec!["y"]),
        Action::Settle(QUIET),
        Action::Screenshot("1-live"),
    ]);
    leave_desktop(&mut steps);
    Interaction { steps }
}

/// Assistant's idle status line.
const HINT_TEXT: &str = "Ask in words, or type search/list/inspect/create/undo.";

fn check_busy_desktop(img: &screen::Image) -> Result<String, String> {
    let title = title(img, "Files", true)?;
    let sel = selected(img, "Photos/", true)?;
    no_selected(img, "Documents/", true)?;
    let busy = content(img, "Thinking: ")?;
    Ok(format!(
        "while the model works (status `Thinking:` at {busy:?}), Files took focus at {title:?} and its selection moved to the 3rd entry at {sel:?}"
    ))
}

fn check_cancelled(img: &screen::Image) -> Result<String, String> {
    let done = content(img, "Cancelled.")?;
    let hint = content(img, HINT_TEXT)?;
    no_content(img, "Thinking: ")?;
    Ok(format!(
        "`Cancelled.` at {done:?}, ready again at {hint:?}, no longer thinking"
    ))
}

fn check_live(img: &screen::Image) -> Result<String, String> {
    let s = content(img, "Assistant: search pancake recipe")?;
    content(img, "  Documents/recipes/pancakes.txt")?;
    let c = content(img, "Assistant: create Projects/2026")?;
    let made = content(img, "  created Projects/2026")?;
    no_content(img, "Assistant: list")?;
    Ok(format!(
        "model chose `search pancake recipe` at {s:?} (result shown) and `create Projects/2026` at {c:?}, created after `y` at {made:?}"
    ))
}

/// docs/s1-design.md §6: seal-test is crashed, hung, and crashed twice
/// more; the fourth end exceeds its limit of three restarts.
fn services_expectation(normal: &Expectation) -> Expectation {
    let crash = |pid: u32| {
        format!(
            "ZOO_OS PROC crash pid={pid} name=seal-test fault=page-fault access=read cause=protection addr=0xffffc00000001000"
        )
    };
    let (c23, c25, c26) = (crash(25), crash(27), crash(28));
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=services",
        // S3: seal-test reaches BeaverHandler by name through Zookeeper.
        "ZOO_OS SHELL line=poke seal-test connect",
        "ZOO_OS SHELL line=poke seal-test crash",
        &c23,
        "ZOO_OS PROC start pid=26 name=seal-test",
        "ZOO_OS SHELL line=services",
        "ZOO_OS SHELL line=poke seal-test hang",
        "ZOO_OS PROC killed pid=26 name=seal-test by=22",
        "ZOO_OS PROC start pid=27 name=seal-test",
        "ZOO_OS SHELL line=services",
        "ZOO_OS SHELL line=poke seal-test crash",
        &c25,
        "ZOO_OS PROC start pid=28 name=seal-test",
        "ZOO_OS SHELL line=poke seal-test crash",
        &c26,
        "ZOO_OS SHELL line=services",
        "ZOO_OS SHELL line=logs",
        // S2: any program's output reaches BeaverHandler.
        "ZOO_OS SHELL line=run puffin-test",
        "ZOO_OS PROC start pid=29 name=puffin-test",
        "ZOO_OS PROC exit pid=29 name=puffin-test code=0",
        "ZOO_OS SHELL line=logs puffin",
        // S4b: the filesystem service is restarted and serves a new root.
        "ZOO_OS SHELL line=restart badger-handler",
        "ZOO_OS PROC killed pid=23 name=badger-handler by=22",
        "ZOO_OS PROC start pid=30 name=badger-handler",
        "ZOO_OS SHELL line=services",
    ]);
    e.clone()
}

fn services_steps() -> Interaction {
    let say = |cmd: &str| Action::Keys(typed(cmd));
    let wait = |line: &str| Action::WaitFor(line.into());
    Interaction {
        steps: vec![
            wait("ZOO_OS SHELL ready"),
            say("services"),
            wait("beaver-handler pid=24 running restarts=0 last=none"),
            wait("seal-test pid=25 running restarts=0 last=none"),
            say("poke seal-test connect"),
            Action::WaitContains(
                "seal-test: connected to beaver-handler by name; it returned".into(),
            ),
            Action::Settle(QUIET),
            say("poke seal-test crash"),
            wait("ZOO_OS PROC start pid=26 name=seal-test"),
            say("services"),
            wait("seal-test pid=26 running restarts=1 last=crashed"),
            // A hang is only noticed by the health check (ping every 1 s,
            // answer within 2 s).
            say("poke seal-test hang"),
            wait("ZOO_OS PROC start pid=27 name=seal-test"),
            say("services"),
            wait("seal-test pid=27 running restarts=2 last=unresponsive"),
            say("poke seal-test crash"),
            wait("ZOO_OS PROC start pid=28 name=seal-test"),
            say("poke seal-test crash"),
            wait(&crash_line_last()),
            Action::Settle(QUIET),
            say("services"),
            wait("seal-test pid=28 failed restarts=3 last=crashed"),
            say("logs"),
            Action::WaitContains("seal-test unresponsive; restarting (2 of 3)".into()),
            Action::WaitContains("seal-test crashed; restart limit reached, left stopped".into()),
            Action::Settle(QUIET),
            say("run puffin-test"),
            wait("ZOO_OS PROC exit pid=29 name=puffin-test code=0"),
            Action::Settle(QUIET),
            say("logs puffin"),
            // BeaverHandler's format (`[secs] [pid:name] text`), not the
            // program's own console line.
            Action::WaitContains("] [29:puffin-test] PuffinTest: all C checks passed".into()),
            Action::Settle(QUIET),
            say("restart badger-handler"),
            wait("ZOO_OS PROC start pid=30 name=badger-handler"),
            Action::WaitContains("Zookeeper: filesystem root from badger-handler (pid 30)".into()),
            Action::Settle(QUIET),
            say("services"),
            wait("badger-handler pid=30 running restarts=1 last=killed"),
            Action::Settle(QUIET),
        ],
    }
}

fn crash_line_last() -> String {
    "ZOO_OS PROC crash pid=28 name=seal-test fault=page-fault access=read cause=protection addr=0xffffc00000001000".into()
}

/// docs/s5-design.md §4: three boots of one machine (one shared data disk).
fn persist_case(normal: &Expectation, name: &'static str, step: u8, timeout: Duration) -> Case {
    let mut e = normal.clone();
    let say = |cmd: &str| Action::Keys(typed(cmd));
    let wait = |line: &str| Action::WaitFor(line.into());
    let contains = |part: &str| Action::WaitContains(part.into());
    // Loading happens while the supervisor starts and may finish just
    // before or just after `SHELL ready`, so these lines are matched
    // anywhere in the log (a fixed order made persist-3 flaky).
    let seen = |part: &str| Action::WaitSeen(part.into());
    let mut steps = match step {
        1 => vec![],
        2 => vec![seen("BadgerHandler: loaded generation 2 from slot B")],
        // persist-3 left generation 1 in slot A and a damaged slot B.
        4 => vec![seen("BadgerHandler: loaded generation 1 from slot A")],
        // persist-4 saved generation 2 to slot B, which is damaged now.
        5 => vec![
            seen("BadgerHandler: slot B: generation 2 failed its check, ignored"),
            seen("BadgerHandler: loaded generation 1 from slot A"),
        ],
        _ => vec![
            seen("BadgerHandler: slot B: generation 2 failed its check, ignored"),
            seen("BadgerHandler: loaded generation 1 from slot A"),
        ],
    };
    steps.push(wait("ZOO_OS SHELL ready"));
    let (disk, purpose) = match step {
        1 => {
            e.then(&[
                "ZOO_OS SHELL line=put Documents/kept.txt first version",
                "ZOO_OS SHELL line=put Documents/kept.txt second version",
                "ZOO_OS SHELL line=cat Documents/kept.txt",
            ]);
            steps.extend([
                say("put Documents/kept.txt first version"),
                contains("BadgerHandler: saved generation 1 to slot A"),
                say("put Documents/kept.txt second version"),
                contains("BadgerHandler: saved generation 2 to slot B"),
                say("cat Documents/kept.txt"),
                wait("second version"),
            ]);
            (
                Disk::Shared {
                    name: "persist",
                    reset: true,
                    corrupt: false,
                },
                "fresh disk: a file is written twice; generations 1 and 2 are saved to slots A and B",
            )
        }
        2 => {
            e.then(&[
                "ZOO_OS SHELL line=cat Documents/kept.txt",
                "ZOO_OS SHELL line=dir Documents",
            ]);
            steps.extend([
                say("cat Documents/kept.txt"),
                wait("second version"),
                say("dir Documents"),
                wait("  kept.txt (15 bytes)"),
            ]);
            (
                Disk::Shared {
                    name: "persist",
                    reset: false,
                    corrupt: false,
                },
                "reboot: the newest generation is loaded and the file reads back",
            )
        }
        4 => {
            e.then(&[
                "ZOO_OS SHELL line=put Documents/kept.txt fourth version",
                "ZOO_OS SHELL line=poke badger-handler half-save",
                "ZOO_OS SHELL line=put Documents/kept.txt fifth version",
                "ZOO_OS PROC exit pid=23 name=badger-handler code=41",
                "ZOO_OS PROC start pid=26 name=badger-handler",
                "ZOO_OS SHELL line=cat Documents/kept.txt",
            ]);
            steps.extend([
                say("put Documents/kept.txt fourth version"),
                contains("BadgerHandler: saved generation 2 to slot B"),
                say("poke badger-handler half-save"),
                Action::Settle(QUIET),
                say("put Documents/kept.txt fifth version"),
                contains("stopping after the payload of generation 3 (simulated power loss)"),
                wait("ZOO_OS PROC start pid=26 name=badger-handler"),
                contains("BadgerHandler: loaded generation 2 from slot B"),
                Action::Settle(QUIET),
                say("cat Documents/kept.txt"),
                wait("fourth version"),
            ]);
            (
                Disk::Shared {
                    name: "persist",
                    reset: false,
                    corrupt: false,
                },
                "a save cut short may not overwrite the blocks of the generation before the current one",
            )
        }
        5 => {
            e.then(&["ZOO_OS SHELL line=cat Documents/kept.txt"]);
            steps.extend([say("cat Documents/kept.txt"), wait("first version")]);
            (
                Disk::Shared {
                    name: "persist",
                    reset: false,
                    corrupt: true,
                },
                "with the newest generation damaged, the one before it reads back intact although a later save was cut short",
            )
        }
        _ => {
            e.then(&[
                "ZOO_OS SHELL line=cat Documents/kept.txt",
                "ZOO_OS SHELL line=poke badger-handler half-save",
                "ZOO_OS SHELL line=put Documents/kept.txt third version",
                "ZOO_OS PROC exit pid=23 name=badger-handler code=41",
                "ZOO_OS PROC start pid=26 name=badger-handler",
                "ZOO_OS SHELL line=cat Documents/kept.txt",
            ]);
            steps.extend([
                say("cat Documents/kept.txt"),
                wait("first version"),
                say("poke badger-handler half-save"),
                Action::Settle(QUIET),
                say("put Documents/kept.txt third version"),
                contains("stopping after the payload of generation 2 (simulated power loss)"),
                wait("ZOO_OS PROC start pid=26 name=badger-handler"),
                contains("BadgerHandler: loaded generation 1 from slot A"),
                Action::Settle(QUIET),
                say("cat Documents/kept.txt"),
                wait("first version"),
            ]);
            (
                Disk::Shared {
                    name: "persist",
                    reset: false,
                    corrupt: true,
                },
                "damaged newest snapshot is rejected; a save cut short by a simulated power loss leaves the previous one",
            )
        }
    };
    steps.push(Action::Settle(QUIET));
    Case {
        name,
        mode: Mode::Normal,
        custom_image: None,
        disk,
        usb: Usb::None,
        expectation: e,
        timeout,
        want: "pass",
        purpose,
        interaction: Some(Interaction { steps }),
        screen_checks: vec![],
    }
}

fn desktop_steps() -> Interaction {
    Interaction {
        steps: vec![
            Action::WaitFor("ZOO_OS SHELL ready".into()),
            Action::Keys(vec!["d", "e", "s", "k", "t", "o", "p", "ret"]),
            Action::WaitFor("ZOO_OS PROC start pid=30 name=assistant".into()),
            Action::Settle(QUIET),
            Action::Screenshot("1-desktop"),
            // Files: down twice, open the details view.
            Action::Keys(vec!["down", "down", "ret"]),
            Action::Settle(QUIET),
            Action::Screenshot("2-details"),
            // Focus Crash Test and make it crash.
            Action::Keys(vec!["tab", "ret"]),
            Action::WaitFor("ZOO_OS PROC start pid=31 name=crash-test".into()),
            Action::Settle(QUIET),
            Action::Screenshot("3-app-restart"),
            // Test key: crash the window server itself.
            Action::Keys(vec!["f12"]),
            Action::WaitFor("ZOO_OS PROC start pid=35 name=assistant".into()),
            Action::Settle(QUIET),
            Action::Screenshot("4-server-restart"),
            // Leave the desktop and use the kernel shell again.
            Action::Keys(vec!["esc"]),
            Action::WaitFor("ZOO_OS DISPLAY released pid=32".into()),
            Action::Settle(QUIET),
            Action::Keys(vec!["e", "c", "h", "o", "spc", "b", "a", "c", "k", "ret"]),
            Action::WaitFor("ZOO_OS SHELL line=echo back".into()),
            Action::Settle(QUIET),
            Action::Screenshot("5-console"),
        ],
    }
}

// ---- D1 mouse case (docs/d1-design.md §3) -----------------------------

/// The desktop started, used with the mouse, then left with Escape.
fn mouse_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=assistant",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=assistant code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

/// The drag in the mouse case: the Assistant's title bar, moved by this.
const DRAG: (i32, i32) = (-400, 60);

fn moved_to(label: &str, k: usize, (dx, dy): (i32, i32)) -> String {
    let w = win(k);
    format!(
        "PeacockHandler: moved {label} to ({}, {})",
        w.0 as i32 + dx,
        w.1 as i32 + dy
    )
}

fn mouse_steps() -> Interaction {
    let mut p = Ptr::start();
    Interaction {
        steps: vec![
            Action::WaitFor("ZOO_OS SHELL ready".into()),
            Action::Keys(vec!["d", "e", "s", "k", "t", "o", "p", "ret"]),
            Action::WaitFor("ZOO_OS PROC start pid=30 name=assistant".into()),
            Action::Settle(QUIET),
            Action::Screenshot("1-pointer"),
            // Into the Crash Test window and click.
            p.to(inside(CRASH_WIN)),
            Action::Button(true),
            Action::Button(false),
            Action::WaitContains("PeacockHandler: click focuses Crash Test".into()),
            Action::Settle(QUIET),
            Action::Screenshot("2-click"),
            // Onto the Assistant's title bar, then drag it.
            p.to(title_bar(ASSISTANT_WIN)),
            Action::Button(true),
            p.by(DRAG.0, DRAG.1),
            Action::Button(false),
            Action::WaitContains(moved_to("Assistant", ASSISTANT_WIN, DRAG)),
            Action::Settle(QUIET),
            Action::Screenshot("3-drag"),
            Action::Keys(vec!["esc"]),
            Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
        ],
    }
}

// ---- D2 partial-redraw cases (docs/d2-design.md §3-4) ------------------

fn start_desktop() -> Vec<Action> {
    vec![
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(vec!["d", "e", "s", "k", "t", "o", "p", "ret"]),
        Action::WaitFor("ZOO_OS PROC start pid=30 name=assistant".into()),
        Action::Settle(QUIET),
    ]
}

/// An animation's end line: at least `min_frames` frames, and lasting
/// `ms` give or take (docs/u2d-design.md).
fn animation_done(what: &str, min_frames: u64, ms: (u64, u64)) -> Action {
    Action::WaitNumbers(
        format!("PeacockHandler: animation {what}:"),
        vec![("frames", min_frames, 10_000), ("time", ms.0, ms.1)],
    )
}

fn animation_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        // Crash Test's red light: it closes.
        p.to(zui::light_centre(win(CRASH_WIN), 0)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: closed Crash Test".into()),
        animation_done("close Crash Test", 3, (170, 1500)),
        // Frame costs from here (F11 reports and restarts them).
        Action::Keys(vec!["f11"]),
        Action::WaitContains("PeacockHandler: frames".into()),
        // Files' yellow light: it goes to the dock.
        p.to(zui::light_centre(win(FILES_WIN), 1)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: minimized Files".into()),
        animation_done("minimize Files", 3, (310, 1500)),
        // Its thumbnail, after the dock's divider: it comes back.
        p.to((mini_centre(1, 0).0, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        p.to(mini_centre(1, 0)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: showed Files".into()),
        animation_done("restore Files", 3, (270, 1500)),
        // The frames of the minimize and restore animations.
        Action::Keys(vec!["f11"]),
        Action::WaitNumbers(
            "PeacockHandler: frames".into(),
            vec![("frames", 6, 100_000)],
        ),
        // Crash Test's dock icon: it opens again.
        // The dock hides itself: reveal it at the bottom edge first.
        p.to((dock_centre(1).0, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        p.to(dock_centre(1)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: opened Crash Test".into()),
        animation_done("open Crash Test", 3, (210, 1500)),
        p.to(inside(FILES_WIN)),
        Action::Settle(QUIET),
        Action::Screenshot("1-after"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

/// After the animations: Crash Test (opened last) focused, Files back in
/// place, nothing left over from the animations (the comparison with a
/// full repaint is the `redraw` case's job).
fn check_after_animations(img: &screen::Image) -> Result<String, String> {
    let t = title(img, "Crash Test", true)?;
    let files = title(img, "Files", false)?;
    let want = title_pen(win(FILES_WIN), "Files");
    if files != want {
        return Err(format!("`Files` title at {files:?}, expected {want:?}"));
    }
    Ok(format!(
        "`Crash Test` focused at {t:?}; `Files` back at {files:?}"
    ))
}

fn genie_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        p.to(zui::light_centre(win(FILES_WIN), 1)),
        // Shift held: the minimise takes 5.2 s instead of 0.52 s.
        Action::Key("shift", true),
        Action::Button(true),
        Action::Button(false),
        Action::Key("shift", false),
        Action::WaitContains("PeacockHandler: minimized Files".into()),
        // About 1.2 s in: still the first phase (the first 40 %).
        Action::Settle(QUIET),
        Action::Screenshot("1-genie"),
        animation_done("minimize Files", 30, (5000, 8000)),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

/// Early in the genie: the window's top edge is where it was, full width
/// (its title bar still spans it), but its bottom-left corner has pinched
/// in, showing wallpaper. A window shrinking toward the dock would have
/// moved its top edge too.
fn check_genie(img: &screen::Image) -> Result<String, String> {
    let (x, y, w, h) = win(FILES_WIN);
    // Files is focused when it is minimised: the snapshot has its title
    // bar in the focused colour.
    let bar = LIGHT.title;
    let bar = [(bar >> 16) as u8, (bar >> 8) as u8, bar as u8];
    let near = |p: Option<[u8; 3]>, c: [u8; 3]| {
        p.is_some_and(|p| p.iter().zip(c).all(|(&a, b)| a.abs_diff(b) <= 6))
    };
    // Title bar colour at both ends of the top edge.
    let (tl, tr) = (img.pixel(x + 40, y + 6), img.pixel(x + w - 40, y + 6));
    if !near(tl, bar) || !near(tr, bar) {
        return Err(format!(
            "the top edge moved: title bar {bar:?} expected at both ends, found {tl:?} and {tr:?}"
        ));
    }
    // Wallpaper, not window, at the bottom-left corner.
    let corner = img.pixel(x + 12, y + h - 12);
    let wall = img.pixel(x - 40, y + h - 12);
    if corner != wall {
        return Err(format!(
            "the bottom-left corner has not pinched in: {corner:?}, wallpaper beside it {wall:?}"
        ));
    }
    Ok(format!(
        "genie, first phase: title bar still across the top at {:?}, wallpaper at the bottom-left corner {:?}",
        (x, y),
        (x + 12, y + h - 12)
    ))
}

fn redraw_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        // From the centre into Crash Test and click: focus changes.
        p.to(inside(CRASH_WIN)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: click focuses Crash Test".into()),
        // Onto the Files title bar and drag it by (100, 50) in four moves:
        // each must clean up where the window was (docs/u2-compositor-
        // design.md).
        p.to(title_bar(FILES_WIN)),
        Action::Button(true),
        p.by(25, 12),
        p.by(25, 13),
        p.by(25, 12),
        p.by(25, 13),
        Action::Button(false),
        Action::WaitContains(moved_to("Files", FILES_WIN, (100, 50))),
        Action::Keys(vec!["tab"]),
    ]);
    // Focus changes and drags of large windows become full repaints, so
    // the pointer wanders last: each old position must be cleaned up by a
    // partial redraw (over the desktop, the windows and their borders).
    for _ in 0..8 {
        steps.push(p.by(45, 40));
    }
    // Then down across a line of Crash Test's text two pixels at a time:
    // each redraw clips the glyphs at another height, so text skipped
    // because it seemed outside the redrawn area would show (U1 draws only
    // what reaches into it, docs/u1-design.md §4).
    // It ends with its 17-pixel area over only the top two rows of the
    // capitals (Inter's reach 10 pixels above the baseline; the area ends 8
    // above it), where such a mistake would leave them missing.
    let c = win(CRASH_WIN);
    let baseline = zui::line_top(c, 0) + zui::BASELINE;
    steps.push(p.to((c.0 + 60, baseline - 45)));
    for _ in 0..10 {
        steps.push(p.by(1, 2));
    }
    steps.extend([
        Action::Settle(QUIET),
        Action::Screenshot("1-partial"),
        Action::Keys(vec!["f10"]),
        Action::WaitContains("PeacockHandler: full repaint".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-full"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

/// Pointer moves, each its own frame: 20 to the right, then 20 back.
/// After a sweep and F11: the sweep's frame report, then mouse events that
/// reached the window server well within a tick (docs/u1b-design.md).
fn input_wait_checks(steps: &mut Vec<Action>) {
    input_wait_checks_frames(steps, 20, 1_000_000);
}

/// As `input_wait_checks`, with the sweep's frame count in `min..=max`
/// (with a hardware cursor, moving the pointer redraws nothing).
fn input_wait_checks_frames(steps: &mut Vec<Action>, min: u64, max: u64) {
    // The sweep's own report (an earlier F11 printed the reset before it).
    steps.push(Action::WaitNumbers(
        "PeacockHandler: frames".into(),
        vec![("frames", min, max)],
    ));
    steps.push(Action::WaitNumbers(
        "PeacockHandler: input wait".into(),
        vec![
            ("input wait", 20, 10_000),
            ("tick: mean", 0, 250),
            ("longest", 0, 500),
        ],
    ));
}

/// A tablet position for screen pixel (x, y): the inverse of the window
/// server's rounding, so the pointer lands on exactly that pixel.
fn abs_at((x, y): (usize, usize)) -> Action {
    let to = |v: usize, n: usize| ((v * 0x7FFF + (n - 1) / 2) / (n - 1)) as u32;
    Action::MouseAbs(to(x, SCREEN.0), to(y, SCREEN.1))
}

/// Drags Files with 400 tablet positions sent back to back (a fast hand
/// on a high-rate mouse), then checks how long events waited.
fn fast_drag_steps() -> Interaction {
    let mut steps = start_desktop();
    steps.push(Action::WaitSeen("PeacockHandler: hardware cursor".into()));
    let (tx, ty) = title_bar(FILES_WIN);
    steps.push(abs_at((tx, ty)));
    steps.push(Action::Keys(vec!["f11"]));
    steps.push(Action::WaitContains("PeacockHandler: frames".into()));
    steps.push(Action::Button(true));
    let to = |x: usize, n: usize| ((x * 0x7FFF + (n - 1) / 2) / (n - 1)) as u32;
    let mut points = Vec::new();
    for k in 0..400usize {
        // Out and back twice: 0..600 px right, 0..300 px down.
        let t = k % 200;
        let d = if t < 100 { t } else { 200 - t };
        points.push((to(tx + 6 * d, SCREEN.0), to(ty + 3 * d, SCREEN.1)));
    }
    steps.push(Action::MouseAbsBurst(points));
    steps.push(Action::Button(false));
    steps.push(Action::WaitContains(
        "PeacockHandler: moved Files to".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["f11"]));
    steps.push(Action::WaitNumbers(
        "PeacockHandler: frames".into(),
        vec![("frames", 1, 1_000_000)],
    ));
    steps.push(Action::WaitNumbers(
        "PeacockHandler: input wait".into(),
        vec![("input wait", 100, 100_000), ("longest", 0, 1_000_000)],
    ));
    Interaction { steps }
}

fn tablet_steps() -> Interaction {
    tablet_steps_with(false)
}

/// A 2560x1440 host: the desktop at 1x over the whole screen, laid out for
/// that size, and a tablet click on Crash Test's close light closes it.
/// Where the drag in `gpu-drag-cursor` leaves the pointer: Files' title
/// bar (right of its title), moved by (120, 60).
fn drag_cursor_end() -> (usize, usize) {
    let (x, y) = title_bar(FILES_WIN);
    (x + 120, y + 60)
}

fn drag_cursor_steps() -> Interaction {
    let mut steps = start_desktop();
    let (x, y) = title_bar(FILES_WIN);
    steps.extend([
        Action::WaitSeen("PeacockHandler: hardware cursor".into()),
        abs_at((x, y)),
        Action::Button(true),
        abs_at((x + 40, y + 20)),
        abs_at((x + 80, y + 40)),
        abs_at(drag_cursor_end()),
        Action::WaitContains("PeacockHandler: pointer drawn in the frame".into()),
        Action::Settle(QUIET),
        Action::Screenshot("1-dragging"),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: moved Files to".into()),
        Action::WaitContains("PeacockHandler: pointer hardware".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-released"),
    ]);
    Interaction { steps }
}

/// The arrow at (x, y): white outline left of the hot spot, black inside.
fn arrow_at(img: &screen::Image, (x, y): (usize, usize)) -> bool {
    let bright = |p: Option<[u8; 3]>| p.is_some_and(|p| p.iter().all(|&v| v >= 230));
    let dark = |p: Option<[u8; 3]>| p.is_some_and(|p| p.iter().all(|&v| v <= 25));
    bright(img.pixel(x - 1, y + 6)) && dark(img.pixel(x + 2, y + 6))
}

fn check_drag_pointer_drawn(img: &screen::Image) -> Result<String, String> {
    let at = drag_cursor_end();
    if !arrow_at(img, at) {
        return Err(format!("mid-drag, no arrow drawn in the frame at {at:?}"));
    }
    Ok(format!(
        "mid-drag: the arrow is in the frame at {at:?}, with the window"
    ))
}

fn check_drag_pointer_gone(img: &screen::Image) -> Result<String, String> {
    let at = drag_cursor_end();
    if arrow_at(img, at) {
        return Err(format!(
            "after the drag, an arrow is still drawn in the frame at {at:?}"
        ));
    }
    Ok(format!(
        "after the drag: no arrow in the frame at {at:?} (the hardware cursor is back)"
    ))
}

fn q1440_steps() -> Interaction {
    let (sw, sh) = (2560, 1440);
    let crash = zui::initial_windows(sw, sh)[CRASH_WIN];
    let (lx, ly) = zui::light_centre(crash, 0);
    let to = |v: usize, n: usize| ((v * 0x7FFF + (n - 1) / 2) / (n - 1)) as u32;
    let mut steps = start_desktop();
    steps.extend([
        Action::WaitSeen("ZOO_OS INFO gpu host size 2560x1440".into()),
        Action::WaitSeen("ZOO_OS INFO gpu scanout 2560x1440 hardware-cursor".into()),
        Action::WaitSeen("PeacockHandler: display 2560x1440 acquired".into()),
        Action::MouseAbs(to(lx, sw), to(ly, sh)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: closed Crash Test".into()),
        Action::Settle(QUIET),
        Action::Screenshot("1-1440"),
    ]);
    Interaction { steps }
}

fn check_1440(img: &screen::Image) -> Result<String, String> {
    if (img.width, img.height) != (2560, 1440) {
        return Err(format!(
            "screen {}x{}, expected 2560x1440",
            img.width, img.height
        ));
    }
    let t = title(img, "Files", true)?;
    let want = title_pen(zui::initial_windows(2560, 1440)[FILES_WIN], "Files");
    if t != want {
        return Err(format!(
            "`Files` title at {t:?}, expected {want:?} (the 2560x1440 layout)"
        ));
    }
    no_title(img, "Crash Test", false)?;
    Ok(format!(
        "2560x1440 at 1x: `Files` at {t:?}; Crash Test closed"
    ))
}

/// At 2x: the host size arrives, the window server draws 2x, and a tablet
/// click on Crash Test's close light (in logical pixels; the tablet's
/// range covers the whole screen either way) closes it.
fn hidpi_steps() -> Interaction {
    let mut steps = start_desktop();
    steps.extend([
        Action::WaitSeen("ZOO_OS INFO gpu host size 3840x2160".into()),
        Action::WaitSeen("ZOO_OS INFO gpu scanout 3840x2160 hardware-cursor".into()),
        Action::WaitSeen("PeacockHandler: 2x scale, 1920x1080 logical".into()),
        abs_at(zui::light_centre(win(CRASH_WIN), 0)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: closed Crash Test".into()),
        Action::Settle(QUIET),
        Action::Screenshot("1-hidpi"),
    ]);
    Interaction { steps }
}

/// The desktop at 2x: the 3840x2160 screenshot has the 26 px faces where
/// the 13 px ones would be, and not the 13 px ones.
fn check_hidpi(img: &screen::Image) -> Result<String, String> {
    if (img.width, img.height) != (3840, 2160) {
        return Err(format!(
            "screen {}x{}, expected 3840x2160",
            img.width, img.height
        ));
    }
    let (fg, bg) = title_colours(true);
    let t = ui(img, FaceId::Medium26, "Files", fg, bg)?;
    let home = ui(img, FaceId::Regular26, "Home", LIGHT.text, LIGHT.content)?;
    let bar = ui(
        img,
        FaceId::SemiBold26,
        "Files",
        LIGHT.bar_text,
        zui::bar_color(&LIGHT),
    )?;
    if screen::find_ui(img, FaceId::Medium13, "Files", fg, bg).is_some() {
        return Err("a 13 px `Files` title at 2x".into());
    }
    // The 1x layout, doubled.
    let want = title_pen(win(FILES_WIN), "Files");
    if t.0.abs_diff(2 * want.0) > 4 || t.1.abs_diff(2 * want.1) > 4 {
        return Err(format!(
            "2x `Files` title at {t:?}, expected near {:?}",
            (2 * want.0, 2 * want.1)
        ));
    }
    no_title(img, "Crash Test", false)?;
    let logo = logo_at(img, 2)?;
    Ok(format!(
        "2x: title `Files` at {t:?}, `Home` at {home:?}, menu bar `Files` at {bar:?}; Crash Test closed; {logo}"
    ))
}

/// The tablet case's steps; `gpu`: on a virtio GPU with a hardware cursor,
/// where the sweep redraws nothing.
fn tablet_steps_with(gpu: bool) -> Interaction {
    let mut steps = start_desktop();
    if gpu {
        steps.push(Action::WaitSeen(
            "ZOO_OS INFO gpu scanout 1920x1080 hardware-cursor".into(),
        ));
        steps.push(Action::WaitSeen("PeacockHandler: hardware cursor".into()));
    }
    steps.extend([
        Action::WaitSeen("ZOO_OS INFO usb pointer absolute x=0..32767 y=0..32767".into()),
        Action::WaitSeen("ZOO_OS INFO usb interrupts=msix".into()),
        // Straight onto Crash Test's red light (12 px across): it closes.
        abs_at(zui::light_centre(win(CRASH_WIN), 0)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("ZOO_OS INFO usb pointer input".into()),
        Action::WaitContains("PeacockHandler: closed Crash Test".into()),
        Action::Settle(QUIET),
        Action::Keys(vec!["f11"]),
        Action::WaitContains("PeacockHandler: frames".into()),
    ]);
    // The pointer sweep, as absolute positions.
    let (x0, y0) = inside(FILES_WIN);
    for k in 0..40 {
        let x = if k < 20 {
            x0 + 12 * k
        } else {
            x0 + 12 * (40 - k)
        };
        steps.push(abs_at((x, y0)));
    }
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["f11"]));
    if gpu {
        // Moving a hardware cursor redraws nothing; the menu-bar clock
        // still ticks once a second during the sweep.
        input_wait_checks_frames(&mut steps, 0, 5);
        steps.push(Action::Settle(QUIET));
        steps.push(Action::Screenshot("1-gpu-desktop"));
    } else {
        input_wait_checks(&mut steps);
    }
    Interaction { steps }
}

fn pointer_sweep(steps: &mut Vec<Action>) {
    for dx in [12, -12] {
        for _ in 0..20 {
            steps.push(Action::Mouse(dx, 0));
        }
    }
}

/// Full repaints with flushes on the virtio GPU: F9 makes every pointer
/// move repaint the whole screen.
fn gpu_frames_steps() -> Interaction {
    let mut steps = start_desktop();
    steps.push(Action::WaitSeen("PeacockHandler: hardware cursor".into()));
    steps.push(Action::Keys(vec!["f11", "f9"]));
    steps.push(Action::WaitContains("full repaint every frame on".into()));
    steps.push(Action::Keys(vec!["f11"]));
    pointer_sweep(&mut steps);
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["f11"]));
    steps.push(Action::WaitContains("PeacockHandler: frames".into()));
    steps.push(Action::WaitNumbers(
        "PeacockHandler: frames".into(),
        vec![("frames", 20, 1_000_000)],
    ));
    // Back to partial redraws, then a window drag.
    steps.push(Action::Keys(vec!["f9"]));
    steps.push(Action::WaitContains("full repaint every frame off".into()));
    steps.push(Action::Settle(QUIET));
    drag_measure(&mut steps);
    content_measure(&mut steps);
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()));
    Interaction { steps }
}

fn frames_steps() -> Interaction {
    let mut steps = start_desktop();
    // Clear the start-up frames, then time a sweep with full repaints.
    steps.push(Action::Keys(vec!["f11", "f9"]));
    steps.push(Action::WaitContains("full repaint every frame on".into()));
    steps.push(Action::Keys(vec!["f11"]));
    pointer_sweep(&mut steps);
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["f11", "f9"]));
    steps.push(Action::WaitContains("PeacockHandler: frames".into()));
    steps.push(Action::WaitContains("full repaint every frame off".into()));
    // The same sweep with partial redraws.
    steps.push(Action::Keys(vec!["f11"]));
    pointer_sweep(&mut steps);
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["f11"]));
    steps.push(Action::WaitContains("PeacockHandler: frames".into()));
    input_wait_checks(&mut steps);
    drag_measure(&mut steps);
    content_measure(&mut steps);
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()));
    Interaction { steps }
}

/// Frame cost of an application's content update: F11, Down and Up five
/// times each in Files (each press moves its selection), F11. At least 8
/// frames.
fn content_measure(steps: &mut Vec<Action>) {
    steps.push(Action::Keys(vec!["f11"]));
    steps.push(Action::WaitContains("PeacockHandler: frames".into()));
    for _ in 0..5 {
        steps.push(Action::Keys(vec!["down", "up"]));
    }
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["f11"]));
    steps.push(Action::WaitNumbers(
        "PeacockHandler: frames".into(),
        vec![("frames", 8, 1_000_000)],
    ));
}

/// Frame cost of dragging a window (docs/u2-compositor-design.md): F11
/// resets the counts, Files is dragged by its title bar in 30 small moves,
/// F11 reports. At least 20 frames.
fn drag_measure(steps: &mut Vec<Action>) {
    steps.push(Action::Keys(vec!["f11"]));
    steps.push(Action::WaitContains("PeacockHandler: frames".into()));
    let mut p = Ptr::start();
    steps.push(p.to(title_bar(FILES_WIN)));
    steps.push(Action::Button(true));
    for _ in 0..30 {
        steps.push(p.by(6, 3));
    }
    steps.push(Action::Button(false));
    steps.push(Action::WaitContains(
        "PeacockHandler: moved Files to".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["f11"]));
    steps.push(Action::WaitNumbers(
        "PeacockHandler: frames".into(),
        vec![("frames", 20, 1_000_000)],
    ));
}

/// Files, still focused, with its selection two entries down: only its
/// content changed since start-up (docs/u2-compositor-design.md).
fn redraw_content_steps() -> Interaction {
    let mut steps = start_desktop();
    steps.extend([
        Action::Keys(vec!["down", "down"]),
        Action::Settle(QUIET),
        Action::Screenshot("1-content"),
        Action::Keys(vec!["f10"]),
        Action::WaitContains("PeacockHandler: full repaint".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-full"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

fn check_content_moved(img: &screen::Image) -> Result<String, String> {
    let t = title(img, "Files", true)?;
    let sel = selected(img, "Photos/", true)?;
    Ok(format!(
        "`Files` focused at {t:?}; `Photos/` selected at {sel:?}"
    ))
}

fn check_partial(img: &screen::Image) -> Result<String, String> {
    // Clicking the Files title focused Files; Tab then moved on to Crash
    // Test, so both windows now look different from how they started.
    let t = title(img, "Crash Test", true)?;
    no_title(img, "Files", true)?;
    let f = win(FILES_WIN);
    let want = title_pen((f.0 + 100, f.1 + 50, f.2, f.3), "Files");
    let files = title(img, "Files", false)?;
    if files != want {
        return Err(format!(
            "`Files` title at {files:?}, expected {want:?} after the drag"
        ));
    }
    Ok(format!(
        "`Crash Test` focused at {t:?}; `Files` dragged to {files:?}"
    ))
}

/// The forced full repaint must look exactly like the partial redraws
/// before it. The menu bar is left out: its clock may tick in between.
fn check_full_repaint(img: &screen::Image) -> Result<String, String> {
    let previous = PREVIOUS
        .with(|p| p.borrow_mut().take())
        .ok_or("no earlier screenshot to compare with")?;
    if (previous.width, previous.height) != (img.width, img.height) {
        return Err("the two screenshots differ in size".into());
    }
    let mut differ = 0;
    let mut first = None;
    for y in MENU_BAR_H..img.height {
        for x in 0..img.width {
            if img.pixel(x, y) != previous.pixel(x, y) {
                differ += 1;
                first.get_or_insert((x, y));
            }
        }
    }
    let compared = img.width * (img.height - MENU_BAR_H);
    match first {
        None => Ok(format!(
            "identical to the partial redraws: {compared} pixels compared (menu bar excluded)"
        )),
        Some(at) => Err(format!(
            "{differ} of {compared} pixels differ from the partial redraws, first at {at:?}"
        )),
    }
}

const MENU_BAR_H: usize = zui::MENU_H;

// ---- Large screens (docs/usb-boot.md) -----------------------------------

fn check_big_screen(img: &screen::Image) -> Result<String, String> {
    if img.width <= 1920 {
        return Err(format!(
            "the screen is {}x{}; this case needs one wider than 1920",
            img.width, img.height
        ));
    }
    let bar = menu_app(img, "Files")?;
    let t = title(img, "Files", true)?;
    // The menu bar reaches the right edge and the wallpaper the bottom
    // right corner: the whole screen is the desktop.
    let c = zui::bar_color(&LIGHT);
    let edge = img.pixel(img.width - 2, 2);
    let corner = img.pixel(img.width - 2, img.height - 2);
    if edge != Some([(c >> 16) as u8, (c >> 8) as u8, c as u8]) || corner == Some([0, 0, 0]) {
        return Err(format!(
            "not the whole screen: menu bar at the right edge {edge:?}, bottom-right corner {corner:?}"
        ));
    }
    Ok(format!(
        "{}x{} screen used whole: menu bar `Files` at {bar:?}, title at {t:?}, menu bar to the right edge",
        img.width, img.height
    ))
}

// ---- D7 Files (docs/d7-design.md §3) -------------------------------------

fn check_preview(img: &screen::Image) -> Result<String, String> {
    let at = content(img, "Home/Documents/notes.txt")?;
    let first = content(img, "Shopping: flour, eggs, milk.")?;
    content(img, "Call the vet about Zara the zebra.")?;
    content(img, "Backspace: back to the folder")?;
    no_content(img, "Shopping: flour, eggs, milk.?")?;
    Ok(format!(
        "preview of `Home/Documents/notes.txt` at {at:?}: its first line at {first:?}"
    ))
}

fn check_back_up(img: &screen::Image) -> Result<String, String> {
    let sel = selected(img, "Photos/", true)?;
    no_selected(img, "Documents/", true)?;
    no_content(img, "Home/Photos")?;
    Ok(format!(
        "back in `Home` from `Photos` with `Photos/` selected at {sel:?}"
    ))
}

// ---- D5 menu-bar clock (docs/d5-design.md §2) ---------------------------

/// How the menu bar shows Unix time `secs` (UTC), computed on the host
/// with the standard days-to-date algorithm.
fn menu_clock_text(secs: u64) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    format!(
        "{} {} {}  {:02}:{:02} UTC",
        WEEKDAYS[days.rem_euclid(7) as usize],
        day,
        MONTHS[month as usize - 1],
        rem / 3600,
        rem % 3600 / 60
    )
}

fn check_menu_clock(img: &screen::Image) -> Result<String, String> {
    let shot = SHOT_TIME
        .with(|t| t.get())
        .ok_or("the screenshot's file time is unknown")?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    // The minute of the screenshot, or the one before it (the frame may
    // have been drawn just before the minute changed).
    for secs in [shot, shot.saturating_sub(60)] {
        let want = menu_clock_text(secs);
        if let Ok(at) = menu_text(img, &want) {
            if let Ok(wrong) = menu_text(img, &menu_clock_text(secs + 86_400)) {
                return Err(format!("the next day's clock matched at {wrong:?}"));
            }
            return Ok(format!(
                "menu bar shows `{want}` at {at:?}, the host's UTC time of the screenshot"
            ));
        }
    }
    Err(format!(
        "menu bar does not show `{}` or `{}`",
        menu_clock_text(shot),
        menu_clock_text(shot.saturating_sub(60))
    ))
}

#[test]
fn menu_clock_text_known_dates() {
    // 1970-01-01 00:00 was a Thursday; 2026-10-31 06:31 is a Saturday;
    // 2000-02-29 is a leap day (Tuesday).
    assert_eq!(menu_clock_text(0), "Thu 1 Jan  00:00 UTC");
    assert_eq!(menu_clock_text(1_793_428_260), "Sat 31 Oct  06:31 UTC");
    assert_eq!(menu_clock_text(951_782_400), "Tue 29 Feb  00:00 UTC");
}

// ---- D4 Notes cases (docs/d4-design.md §3) -----------------------------

/// The desktop; Notes is opened from the dock, then the session ends.
fn notes_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=assistant",
        "ZOO_OS PROC start pid=31 name=notes",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=assistant code=0",
        "ZOO_OS PROC exit pid=31 name=notes code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

/// `linux-petrel-1` sets up names, links, `*at` calls and `mmap` in the
/// home folder on a fresh disk; `linux-petrel-2` boots the same disk and
/// finds the names and links (docs/linux10d-design.md).
fn petrel_case(normal: &Expectation, first: bool, timeout: Duration) -> Case {
    let mode = if first { "write" } else { "check" };
    let done = if first {
        "PetrelTest: all checks passed"
    } else {
        "PetrelTest: kept across the reboot"
    };
    let mut e = normal.clone();
    let line = format!("ZOO_OS SHELL line=linux petrel-test {mode}");
    e.then(&[
        &line,
        "ZOO_OS PROC start pid=26 name=petrel-test",
        "ZOO_OS PROC exit pid=26 name=petrel-test code=0",
    ]);
    let mut steps = Vec::new();
    if !first {
        steps.push(Action::WaitSeen("BadgerHandler: loaded generation".into()));
    }
    steps.extend([
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(typed(&format!("linux petrel-test {mode}"))),
        Action::WaitContains(done.into()),
        Action::WaitFor("ZOO_OS PROC exit pid=26 name=petrel-test code=0".into()),
    ]);
    if first {
        steps.push(Action::WaitContains(
            "BadgerHandler: saved generation".into(),
        ));
        steps.push(Action::Settle(Duration::from_secs(3)));
    }
    Case {
        name: if first {
            "linux-petrel-1"
        } else {
            "linux-petrel-2"
        },
        mode: Mode::Normal,
        custom_image: None,
        disk: Disk::Shared {
            name: "petrel",
            reset: first,
            corrupt: false,
        },
        usb: Usb::None,
        expectation: e,
        timeout,
        want: "pass",
        purpose: if first {
            "home folder names with marks, 200-byte names, symbolic links, *at calls, mmap and utimensat"
        } else {
            "after a reboot of the same disk the names and links are still there"
        },
        interaction: Some(Interaction { steps }),
        screen_checks: vec![],
    }
}

/// `linux-murre-1` writes 40 MiB and 300 small files on a fresh disk;
/// `linux-murre-2` boots the same disk and reads every byte back
/// (docs/linux10c-design.md).
fn murre_case(normal: &Expectation, name: &'static str, timeout: Duration) -> Case {
    let first = name == "linux-murre-1";
    let mode = if first { "write" } else { "check" };
    let mut e = normal.clone();
    let line = format!("ZOO_OS SHELL line=linux murre-test {mode}");
    e.then(&[
        &line,
        "ZOO_OS PROC start pid=26 name=murre-test",
        "ZOO_OS PROC exit pid=26 name=murre-test code=0",
    ]);
    let mut steps = Vec::new();
    if !first {
        steps.push(Action::WaitSeen("BadgerHandler: loaded generation".into()));
    }
    steps.extend([
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(typed(&format!("linux murre-test {mode}"))),
    ]);
    if first {
        steps.push(Action::WaitFor(
            "ZOO_OS PROC exit pid=26 name=murre-test code=0".into(),
        ));
        // The last changes are saved once BadgerHandler has been quiet.
        steps.push(Action::WaitContains(
            "BadgerHandler: saved generation".into(),
        ));
        steps.push(Action::Settle(Duration::from_secs(3)));
    } else {
        steps.push(Action::WaitContains("MurreTest: all checks passed".into()));
        steps.push(Action::WaitFor(
            "ZOO_OS PROC exit pid=26 name=murre-test code=0".into(),
        ));
    }
    Case {
        name,
        mode: Mode::Normal,
        custom_image: None,
        disk: Disk::Shared {
            name: "murre",
            reset: first,
            corrupt: false,
        },
        usb: Usb::None,
        expectation: e,
        timeout,
        want: "pass",
        purpose: if first {
            "a Linux program writes 40 MiB and 300 small files to the home folder (saves forced part-way)"
        } else {
            "after a reboot of the same disk, every byte of the 40 MiB and small files reads back"
        },
        interaction: Some(Interaction { steps }),
        screen_checks: vec![],
    }
}

/// `notes-1` types into Notes on a fresh disk; `notes-2` boots the same
/// disk again and finds the typing.
fn notes_case(expectation: Expectation, name: &'static str, timeout: Duration) -> Case {
    let first = name == "notes-1";
    let mut steps = Vec::new();
    if !first {
        // Loading may finish just before or after the shell is ready.
        steps.push(Action::WaitSeen(
            "BadgerHandler: loaded generation 1 from slot A".into(),
        ));
    }
    steps.extend(start_desktop());
    let mut p = Ptr::start();
    steps.extend([
        // The Notes icon in the dock.
        // The dock hides itself: reveal it at the bottom edge first.
        p.to((dock_centre(3).0, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        p.to(dock_centre(3)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: opened Notes".into()),
        // Up into the window, away from the dock.
        p.to(inside(3)),
    ]);
    if first {
        steps.extend([
            Action::Keys(typed("Hello from Notes")),
            Action::Keys(typed("second line")),
            Action::WaitContains("Notes: saved".into()),
            Action::WaitContains("BadgerHandler: saved generation 1 to slot A".into()),
        ]);
    }
    steps.extend([
        Action::Settle(QUIET),
        Action::Screenshot("1-notes"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Case {
        name,
        mode: Mode::Normal,
        custom_image: None,
        disk: Disk::Shared {
            name: "notes",
            reset: first,
            corrupt: false,
        },
        usb: Usb::None,
        expectation,
        timeout,
        want: "pass",
        purpose: if first {
            "Notes: open from the dock, type two lines, saved to the home folder and the disk"
        } else {
            "Notes after a reboot of the same disk: the typed lines are back"
        },
        interaction: Some(Interaction { steps }),
        screen_checks: vec![("1-notes", check_notes)],
    }
}

fn check_notes(img: &screen::Image) -> Result<String, String> {
    let title = title(img, "Notes", true)?;
    let head = content(img, "Documents/notes.txt (saved)")?;
    let old = content(img, "Shopping: flour, eggs, milk.")?;
    let typed = content(img, "Hello from Notes")?;
    let second = content(img, "second line")?;
    no_content(img, "Hello from Notez")?;
    // The seed file has CRLF line ends; a shown carriage return is `?`.
    no_content(img, "Shopping: flour, eggs, milk.?")?;
    if !(old.1 < typed.1 && typed.1 < second.1) {
        return Err(format!(
            "lines out of order: original {old:?}, typed {typed:?}, second {second:?}"
        ));
    }
    Ok(format!(
        "`Notes` focused {title:?}, `(saved)` {head:?}; original text {old:?}, then `Hello from Notes` {typed:?} and `second line` {second:?}"
    ))
}

// ---- D10 Notes cursor (docs/d10-design.md) ------------------------------

fn check_notes_cursor(img: &screen::Image) -> Result<String, String> {
    let at = content(img, "aX|bc")?;
    no_content(img, "aXY")?;
    no_content(img, "abcXY")?;
    content(img, "Documents/notes.txt (saved)")?;
    Ok(format!(
        "`aX|bc` at {at:?}: X inserted two places back, Y deleted at the cursor, saved"
    ))
}

// ---- U1 appearance case (docs/u1-design.md) -------------------------------

fn appearance_steps() -> Interaction {
    // The switch sits left of the clock, whose width depends on the time;
    // a minute's change moves it by a pixel or two, inside its hit area.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let clock_w = screen::ui_width(FaceId::Regular13, &menu_clock_text(now));
    let switch = zui::appearance_switch(SCREEN.0, clock_w);
    let mut steps = start_desktop();
    let mut p = Ptr::start();
    steps.extend([
        Action::Keys(vec!["f8"]),
        Action::WaitContains("PeacockHandler: appearance dark".into()),
        Action::Settle(QUIET),
        Action::Screenshot("1-dark"),
        p.to(switch),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: appearance light".into()),
        // Back where nothing changes under the pointer.
        p.to(inside(FILES_WIN)),
        Action::Settle(QUIET),
        Action::Screenshot("2-light"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

fn check_dark(img: &screen::Image) -> Result<String, String> {
    let d = &zui::DARK;
    let t = ui(img, FaceId::Medium13, "Files", d.title_text, d.title)?;
    let home = ui(img, FaceId::Regular13, "Home", d.text, d.content)?;
    let sel = ui(
        img,
        FaceId::Regular13,
        "Documents/",
        d.select_text,
        d.select,
    )?;
    let menu = ui(
        img,
        FaceId::SemiBold13,
        "Files",
        d.bar_text,
        zui::bar_color(d),
    )?;
    ui(
        img,
        FaceId::Medium13,
        "Assistant",
        d.title_text_blurred,
        d.title_blurred,
    )?;
    // Nothing is left in the light appearance.
    no_content(img, "Home")?;
    no_title(img, "Files", true)?;
    if img.pixel(SCREEN.0 / 2, SCREEN.1 - 4)
        != Some({
            let c = zoo_abi::font::blend(
                d.wall_top,
                d.wall_bottom,
                ((SCREEN.1 - 4) * 255 / (SCREEN.1 - 1)) as u8,
            );
            [(c >> 16) as u8, (c >> 8) as u8, c as u8]
        })
    {
        return Err("the wallpaper is not the dark one".into());
    }
    Ok(format!(
        "dark: `Files` title {t:?}, `Home` {home:?}, `Documents/` selected {sel:?}, menu bar {menu:?}, dark wallpaper; no light text left"
    ))
}

fn check_light_again(img: &screen::Image) -> Result<String, String> {
    let report = check_desktop_started(img)?;
    no_ui(
        img,
        FaceId::Regular13,
        "Home",
        zui::DARK.text,
        zui::DARK.content,
    )?;
    Ok(format!("light again after the switch: {report}"))
}

// ---- D8 resize case (docs/d8-design.md) -----------------------------------

fn check_resized(img: &screen::Image) -> Result<String, String> {
    let c = win(CRASH_WIN);
    let now = (c.0, c.1, c.2 - 200, c.3 - 100);
    let want = title_pen(now, "Crash Test");
    let at = title(img, "Crash Test", true)?;
    if at != want {
        return Err(format!(
            "`Crash Test` title at {at:?}, expected {want:?} (centred in the narrower window)"
        ));
    }
    content(img, "This window is a crash test.")?;
    // Where the window's right and bottom parts were: wallpaper now (past
    // its shadow).
    for (x, y) in [(c.0 + c.2 - 40, c.1 + c.3 / 2), (c.0 + 300, c.1 + c.3 - 10)] {
        if img.pixel(x, y) != Some(wallpaper(y)) {
            return Err(format!(
                "at ({x}, {y}), once inside the window: {:?}, not the wallpaper {:?}",
                img.pixel(x, y),
                wallpaper(y)
            ));
        }
    }
    Ok(format!(
        "`Crash Test` focused, now {}x{}: title re-centred at {at:?}; wallpaper where it was",
        now.2, now.3
    ))
}

// ---- D9 zoom case (docs/d9-design.md) -------------------------------------

fn check_zoomed(img: &screen::Image) -> Result<String, String> {
    let want = title_pen(zui::zoomed(SCREEN.0, SCREEN.1), "Assistant");
    let at = title(img, "Assistant", true)?;
    if at != want {
        return Err(format!(
            "`Assistant` title at {at:?}, expected {want:?} when zoomed"
        ));
    }
    no_title(img, "Files", false)?;
    no_title(img, "Crash Test", false)?;
    Ok(format!(
        "`Assistant` zoomed: title at {at:?}, covering Files and Crash Test"
    ))
}

fn check_restored(img: &screen::Image) -> Result<String, String> {
    let want = title_pen(win(ASSISTANT_WIN), "Assistant");
    let at = title(img, "Assistant", true)?;
    if at != want {
        return Err(format!(
            "`Assistant` title at {at:?}, expected {want:?} after restoring"
        ));
    }
    let files = title(img, "Files", false)?;
    Ok(format!(
        "`Assistant` restored to {at:?}; `Files` visible again at {files:?}"
    ))
}

// ---- D3 dock case (docs/d3-design.md §3) -------------------------------

/// Crash Test is closed (it ends normally and is not restarted), then
/// started again from the dock.
/// The `dock-menu` case: a second Files process (pid 31) starts and lasts
/// to the end.
fn dock_menu_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=assistant",
        "ZOO_OS PROC start pid=31 name=files",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=31 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=assistant code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

/// The `apps-folder` case: the desktop runs twice.
fn apps_folder_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    for base in [26, 31] {
        let (z, p, f, c, a) = (base, base + 1, base + 2, base + 3, base + 4);
        e.then(&[
            "ZOO_OS SHELL line=desktop",
            &format!("ZOO_OS PROC start pid={z} name=zookeeper"),
            &format!("ZOO_OS PROC start pid={p} name=peacock-handler"),
            &format!("ZOO_OS DISPLAY acquired pid={p} name=peacock-handler"),
            &format!("ZOO_OS PROC start pid={f} name=files"),
            &format!("ZOO_OS PROC start pid={c} name=crash-test"),
            &format!("ZOO_OS PROC start pid={a} name=assistant"),
            &format!("ZOO_OS PROC exit pid={p} name=peacock-handler code=0"),
            &format!("ZOO_OS DISPLAY released pid={p}"),
        ]);
        e.then_any_order(&[
            &format!("ZOO_OS PROC exit pid={f} name=files code=0"),
            &format!("ZOO_OS PROC exit pid={c} name=crash-test code=0"),
            &format!("ZOO_OS PROC exit pid={a} name=assistant code=0"),
            &format!("ZOO_OS PROC exit pid={z} name=zookeeper code=0"),
        ]);
    }
    e
}

/// Removes Notes (dock slot 3 of 4) through its dock menu.
fn remove_notes(p: &mut Ptr) -> Vec<Action> {
    let notes = [false, true, false];
    vec![
        p.to((dock_centre(3).0, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        p.to(dock_centre(3)),
        Action::RightButton(true),
        Action::RightButton(false),
        Action::WaitContains("PeacockHandler: menu Notes: Open, Remove from Dock".into()),
        p.to(menu_item(3, &notes, 2)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: removed Notes from the dock".into()),
        Action::WaitContains("PeacockHandler: dock saved (3 kept)".into()),
    ]
}

/// The centre-left of Files' content line `k` (first window).
fn files_line(k: usize) -> (usize, usize) {
    let w = win(FILES_WIN);
    (w.0 + 60, zui::line_top(w, k) + zui::LINE_H / 2)
}

fn apps_folder_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend(remove_notes(&mut p));
    // Files has the focus: to the last line of home, Applications, and in.
    steps.push(Action::Keys(vec!["down"; 16]));
    steps.extend([
        Action::Keys(vec!["ret"]),
        p.to((SCREEN.0 / 2, SCREEN.1 / 2)),
        Action::Settle(QUIET),
        Action::Screenshot("1-folder"),
        // Right-click Notes (the fourth application): Open, Add to Dock.
        p.to(files_line(4)),
        Action::RightButton(true),
        Action::RightButton(false),
        Action::WaitContains("PeacockHandler: menu Files: Open, Add to Dock".into()),
    ]);
    let (x, y) = files_line(4);
    let seps = [false, false];
    steps.extend([
        p.to((
            x + 90,
            zui::menu_item_top(y + 2, &seps, 1) + zui::MENU_ITEM_H / 2,
        )),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: menu chose Add to Dock".into()),
        Action::WaitContains("PeacockHandler: kept Notes in the dock".into()),
        Action::WaitContains("PeacockHandler: dock saved (4 kept)".into()),
        p.to((SCREEN.0 / 2, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        Action::Settle(QUIET),
        Action::Screenshot("2-pinned"),
    ]);
    // Out again, then a new desktop: the saved pins come back.
    steps.extend(remove_notes(&mut p));
    steps.extend([
        p.to((SCREEN.0 / 2, SCREEN.1 / 2)),
        Action::Settle(QUIET),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
        Action::WaitFor("ZOO_OS PROC exit pid=26 name=zookeeper code=0".into()),
        Action::Keys(vec!["d", "e", "s", "k", "t", "o", "p", "ret"]),
        Action::WaitContains("PeacockHandler: dock loaded (3 kept)".into()),
        Action::WaitFor("ZOO_OS PROC start pid=35 name=assistant".into()),
        Action::Settle(QUIET),
        p.to((SCREEN.0 / 2, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        Action::Settle(QUIET),
        Action::Screenshot("3-reloaded"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=32".into()),
    ]);
    Interaction { steps }
}

/// Files shows the Applications folder: its location, and Notes listed.
fn check_apps_folder(img: &screen::Image) -> Result<String, String> {
    let place = content(img, "Home/Applications")?;
    let notes = content(img, "Notes")?;
    selected(img, "Files", true)?;
    Ok(format!(
        "Files in Applications: `Home/Applications` at {place:?}, `Notes` at {notes:?}"
    ))
}

/// Notes is back in the dock: the four-icon dock, Notes' yellow in it.
fn check_notes_back(img: &screen::Image) -> Result<String, String> {
    let (x, y) = zui::dock_icon(SCREEN.0, SCREEN.1, 4, 3);
    let (cx, cy) = (x + zui::DOCK_ICON / 2, y + zui::DOCK_ICON / 2);
    let yellow = [0xF5, 0xC3, 0x42];
    let found = (y..y + zui::DOCK_ICON)
        .any(|py| (x..x + zui::DOCK_ICON).any(|px| img.pixel(px, py) == Some(yellow)));
    if !found {
        return Err(format!(
            "no Notes yellow in the fourth dock slot around ({cx}, {cy})"
        ));
    }
    Ok(format!("Notes back in the dock, in slot 4 at ({x}, {y})"))
}

/// The `windows` case: a second Files process (pid 31) starts and ends.
fn windows_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=assistant",
        "ZOO_OS PROC start pid=31 name=files",
        "ZOO_OS PROC exit pid=31 name=files code=0",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=assistant code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

fn dock_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=assistant",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC start pid=31 name=crash-test",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=31 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=assistant code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

/// A running dot under dock icon `k` (4 px, the menu-bar text colour).
fn dock_dot(img: &screen::Image, k: usize) -> bool {
    let (x, y) = zui::dock_icon(SCREEN.0, SCREEN.1, 4, k);
    let c = LIGHT.bar_text;
    img.pixel(x + zui::DOCK_ICON / 2 - 1, y + zui::DOCK_ICON + 3)
        == Some([(c >> 16) as u8, (c >> 8) as u8, c as u8])
}

fn dock_steps() -> Interaction {
    let mut steps = start_desktop();
    let mut p = Ptr::start();
    let close = zui::light_centre(win(CRASH_WIN), 0);
    steps.extend([
        // Onto the Files icon: its name shows above it.
        // The dock hides itself: reveal it at the bottom edge first.
        p.to((dock_centre(0).0, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        p.to(dock_centre(0)),
        Action::Settle(QUIET),
        Action::Screenshot("1-dock"),
        // Crash Test's red light.
        p.to(close),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: closed Crash Test".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-closed"),
        // The Crash Test icon.
        // The dock hides itself: reveal it at the bottom edge first.
        p.to((dock_centre(1).0, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        p.to(dock_centre(1)),
        Action::Settle(QUIET),
        Action::Screenshot("3-dock-closed"),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: opened Crash Test".into()),
        Action::Settle(QUIET),
        Action::Screenshot("4-dock-reopened"),
        // Away from the dock: it hides.
        p.to(inside(FILES_WIN)),
        Action::WaitContains("PeacockHandler: dock hidden".into()),
        Action::Settle(QUIET),
        Action::Screenshot("5-reopened"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

fn check_dock_shown(img: &screen::Image) -> Result<String, String> {
    let name = tooltip(img, "Files")?;
    no_tooltip(img, "Notes")?;
    if !(0..3).all(|i| dock_dot(img, i)) || dock_dot(img, 3) {
        return Err("running dots should mark the first three apps only".into());
    }
    Ok(format!(
        "pointer on the Files icon: its name at {name:?}; running dots under the first three, none under Notes"
    ))
}

fn check_closed(img: &screen::Image) -> Result<String, String> {
    no_title(img, "Crash Test", true)?;
    no_title(img, "Crash Test", false)?;
    no_content(img, "This window is a crash test.")?;
    let files = title(img, "Files", true)?;
    if dock_dot(img, 1) {
        return Err("Crash Test still has a running dot".into());
    }
    Ok(format!(
        "Crash Test window gone and its dot cleared; `Files` still focused at {files:?}"
    ))
}

fn check_dock_closed(img: &screen::Image) -> Result<String, String> {
    tooltip(img, "Crash Test")?;
    if !dock_dot(img, 0) || dock_dot(img, 1) || !dock_dot(img, 2) || dock_dot(img, 3) {
        return Err("dots should mark Files and the Assistant only".into());
    }
    Ok(
        "dock: Crash Test's name shown, no running dot under it, dots under Files and Assistant"
            .into(),
    )
}

fn check_reopened(img: &screen::Image) -> Result<String, String> {
    let t = title(img, "Crash Test", true)?;
    let body = content(img, "This window is a crash test.")?;
    no_title(img, "Files", true)?;
    no_tooltip(img, "Crash Test")?;
    dock_hidden(img)?;
    Ok(format!(
        "Crash Test reopened and focused: title {t:?}, text {body:?}; the dock hidden again"
    ))
}

/// The centre of minimised window `j`'s thumbnail, with `minis` of them,
/// in a dock of all four applications.
fn mini_centre(minis: usize, j: usize) -> (usize, usize) {
    let (x, y) = zui::dock_mini(SCREEN.0, SCREEN.1, 4, minis, j);
    (x + zui::DOCK_ICON / 2, y + zui::DOCK_ICON / 2)
}

fn dock_minis_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        p.to(zui::light_centre(win(FILES_WIN), 1)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: animation minimize Files".into()),
        p.to(zui::light_centre(win(ASSISTANT_WIN), 1)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: animation minimize Assistant".into()),
        // The dock up (the pointer at the bottom edge; it may still be up
        // from the genie).
        p.to((SCREEN.0 / 2 - 300, SCREEN.1 - 1)),
        Action::Settle(QUIET),
        Action::Screenshot("1-minis"),
        // Files' thumbnail (the first): Files comes back, the Assistant
        // stays in the dock.
        p.to(mini_centre(2, 0)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: showed Files".into()),
        Action::WaitContains("PeacockHandler: animation restore Files".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-back"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

/// The divider line after the four applications, then two thumbnails
/// with their applications' badges: Files' blue first, the Assistant's
/// violet second.
fn check_dock_minis(img: &screen::Image) -> Result<String, String> {
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    let (_, y, _, h) = zui::dock_rect_with(SCREEN.0, SCREEN.1, 4, 2);
    let lx = zui::dock_divider_x(SCREEN.0, SCREEN.1, 4, 2);
    if img.pixel(lx, y + h / 2) != Some(rgb(LIGHT.separator)) {
        return Err(format!(
            "no divider at x {lx}: {:?}",
            img.pixel(lx, y + h / 2)
        ));
    }
    for (j, (c, name)) in [(0x3B8EEA, "Files"), (0x7A5AF8, "Assistant")]
        .into_iter()
        .enumerate()
    {
        let (mx, my) = zui::dock_mini(SCREEN.0, SCREEN.1, 4, 2, j);
        let badge = (mx + zui::DOCK_ICON - 7, my + zui::DOCK_ICON - 7);
        if img.pixel(badge.0, badge.1) != Some(rgb(c)) {
            return Err(format!(
                "thumbnail {j} should carry {name}'s badge at {badge:?}: {:?}",
                img.pixel(badge.0, badge.1)
            ));
        }
    }
    Ok(format!(
        "the divider at x {lx}, then thumbnails of Files and the Assistant with their badges"
    ))
}

fn check_files_back(img: &screen::Image) -> Result<String, String> {
    let t = title(img, "Files", true)?;
    no_title(img, "Assistant", false)?;
    no_title(img, "Assistant", true)?;
    Ok(format!(
        "Files back and focused at {t:?}; the Assistant still in the dock"
    ))
}

/// The centre of item `k` of the context menu for dock icon `slot` (of 4),
/// the items' separators being `seps` (docs/u3-design.md).
fn menu_item(slot: usize, seps: &[bool], k: usize) -> (usize, usize) {
    let (ix, iy) = zui::dock_icon(SCREEN.0, SCREEN.1, 4, slot);
    let top = iy - 10 - zui::menu_height(seps);
    // Right of the labels (they start 16 px into a menu at least 160 wide,
    // centred on the icon), so the pointer does not cover them.
    (
        ix + zui::DOCK_ICON / 2 + 50,
        zui::menu_item_top(top, seps, k) + zui::MENU_ITEM_H / 2,
    )
}

fn dock_menu_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    // Files is running: New Window, Remove from Dock, Quit.
    let files = [false, true, false, false];
    // Notes is not: Open, Remove from Dock.
    let notes = [false, true, false];
    steps.extend([
        p.to((dock_centre(0).0, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        p.to(dock_centre(0)),
        Action::RightButton(true),
        Action::RightButton(false),
        Action::WaitContains(
            "PeacockHandler: menu Files: New Window, Remove from Dock, Quit".into(),
        ),
        // Onto New Window: it lights up.
        p.to(menu_item(0, &files, 0)),
        Action::Settle(QUIET),
        Action::Screenshot("1-menu"),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: menu chose New Window".into()),
        Action::WaitContains("PeacockHandler: new window Files (window 4)".into()),
        // Notes: Remove from Dock.
        p.to((dock_centre(3).0, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        p.to(dock_centre(3)),
        Action::RightButton(true),
        Action::RightButton(false),
        Action::WaitContains("PeacockHandler: menu Notes: Open, Remove from Dock".into()),
        p.to(menu_item(3, &notes, 2)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: removed Notes from the dock".into()),
        // Back to the bottom edge, so the dock stays up for the picture.
        p.to((SCREEN.0 / 2, SCREEN.1 - 1)),
        Action::Settle(QUIET),
        Action::Screenshot("2-removed"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

fn check_dock_menu(img: &screen::Image) -> Result<String, String> {
    let on = ui(
        img,
        FaceId::Regular13,
        "New Window",
        LIGHT.select_text,
        LIGHT.select,
    )?;
    let remove = ui(
        img,
        FaceId::Regular13,
        "Remove from Dock",
        LIGHT.tooltip_text,
        LIGHT.tooltip,
    )?;
    let quit = ui(
        img,
        FaceId::Regular13,
        "Quit",
        LIGHT.tooltip_text,
        LIGHT.tooltip,
    )?;
    Ok(format!(
        "the Files menu: `New Window` highlighted at {on:?}, `Remove from Dock` at {remove:?}, `Quit` at {quit:?}"
    ))
}

/// Notes is out of the dock: three icons, and none of Notes' yellow on
/// the row through their middles.
fn check_notes_removed(img: &screen::Image) -> Result<String, String> {
    let (x, y, w, _) = zui::dock_rect(SCREEN.0, SCREEN.1, 3);
    let row = y + zui::DOCK_PAD + zui::DOCK_ICON / 2;
    let yellow = [0xF5, 0xC3, 0x42];
    if (x..x + w).any(|px| img.pixel(px, row) == Some(yellow)) {
        return Err("Notes' yellow is still in the dock".into());
    }
    // The dock is up: Files' running dot, in the three-icon layout.
    let (fx, fy) = zui::dock_icon(SCREEN.0, SCREEN.1, 3, 0);
    let c = LIGHT.bar_text;
    if img.pixel(fx + zui::DOCK_ICON / 2 - 1, fy + zui::DOCK_ICON + 3)
        != Some([(c >> 16) as u8, (c >> 8) as u8, c as u8])
    {
        return Err("the three-icon dock is not shown (no running dot under Files)".into());
    }
    Ok(format!(
        "the dock without Notes: no Notes yellow on row {row} between x {x} and {}",
        x + w
    ))
}

/// The second Files window: the first one's rectangle moved by (32, 32).
fn second_files() -> (usize, usize, usize, usize) {
    let (x, y, w, h) = win(FILES_WIN);
    (x + 32, y + 32, w, h)
}

fn windows_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        // Files has the focus at start: Ctrl+N opens another Files window.
        Action::Key("ctrl", true),
        Action::Keys(vec!["n"]),
        Action::Key("ctrl", false),
        Action::WaitContains("PeacockHandler: new window Files (window 4)".into()),
        Action::WaitContains("PeacockHandler: animation open Files".into()),
        Action::Settle(QUIET),
        Action::Screenshot("1-two"),
        // Its red light closes it; the first Files window stays.
        p.to(zui::light_centre(second_files(), 0)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: closed Files".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-one"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

fn check_two_files(img: &screen::Image) -> Result<String, String> {
    let new = title(img, "Files", true)?;
    let want_new = title_pen(second_files(), "Files");
    let old = title(img, "Files", false)?;
    let want_old = title_pen(win(FILES_WIN), "Files");
    if new != want_new || old != want_old {
        return Err(format!(
            "two Files windows expected: focused at {want_new:?} (found {new:?}), the first at {want_old:?} (found {old:?})"
        ));
    }
    Ok(format!(
        "two Files windows: the new one focused at {new:?}, the first at {old:?}"
    ))
}

fn check_one_files(img: &screen::Image) -> Result<String, String> {
    let t = title(img, "Files", true)?;
    let want = title_pen(win(FILES_WIN), "Files");
    if t != want {
        return Err(format!(
            "the first Files window should be focused at {want:?}, found {t:?}"
        ));
    }
    no_title(img, "Files", false)?;
    Ok(format!("one Files window again, focused at {t:?}"))
}

/// The soft paw at `scale`: the middle of its main pad is in the menu-bar
/// text colour, the gap between its two upper toes in the bar colour (the
/// old mark had a toe there).
fn logo_at(img: &screen::Image, scale: usize) -> Result<String, String> {
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    let (ink, bar) = (rgb(LIGHT.bar_text), rgb(zui::bar_color(&LIGHT)));
    // Design units (50, 66) and (50, 30) in the 18-pixel box at (9, 4).
    let pad = (18 * scale, 16 * scale);
    let gap = (18 * scale, 7 * scale);
    let (p, g) = (img.pixel(pad.0, pad.1), img.pixel(gap.0, gap.1));
    if p != Some(ink) || g != Some(bar) {
        return Err(format!(
            "not the soft paw: main pad {pad:?} is {p:?} (want {ink:?}), toe gap {gap:?} is {g:?} (want {bar:?})"
        ));
    }
    Ok(format!(
        "the soft paw: main pad filled at {pad:?}, gap between the upper toes at {gap:?}"
    ))
}

fn check_logo(img: &screen::Image) -> Result<String, String> {
    logo_at(img, 1)
}

/// Just after reopening, the dock still up: a running dot under Crash Test.
fn check_dock_reopened(img: &screen::Image) -> Result<String, String> {
    if !dock_dot(img, 1) {
        return Err("no running dot under Crash Test".into());
    }
    Ok("the dock still up: a running dot under Crash Test again".into())
}

/// The dock is hidden: wallpaper where its middle would be.
fn dock_hidden(img: &screen::Image) -> Result<(), String> {
    let (x, y, w, h) = zui::dock_rect(SCREEN.0, SCREEN.1, 4);
    let (cx, cy) = (x + w / 2, y + h / 2);
    let p = img.pixel(cx, cy);
    if p != Some(wallpaper(cy)) {
        return Err(format!(
            "the dock should be hidden: {p:?} at {:?}, wallpaper is {:?}",
            (cx, cy),
            wallpaper(cy)
        ));
    }
    Ok(())
}

fn check_dock_hidden(img: &screen::Image) -> Result<String, String> {
    dock_hidden(img)?;
    Ok("the dock hidden: wallpaper where it would be".into())
}

fn check_dock_revealed(img: &screen::Image) -> Result<String, String> {
    if !dock_dot(img, 0) || !dock_dot(img, 1) || !dock_dot(img, 2) || dock_dot(img, 3) {
        return Err("revealed dock: running dots should mark the first three apps".into());
    }
    Ok("the dock revealed at the bottom edge: running dots under the first three apps".into())
}

fn dock_autohide_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        Action::Screenshot("1-hidden"),
        // Near the bottom edge (4 pixels short, as a scaled QEMU window
        // leaves it): the dock slides in.
        p.to((SCREEN.0 / 2 - 200, SCREEN.1 - 5)),
        Action::WaitContains("PeacockHandler: dock shown".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-shown"),
        // Away: it slides out again.
        p.to(inside(FILES_WIN)),
        Action::WaitContains("PeacockHandler: dock hidden".into()),
        Action::Settle(QUIET),
        Action::Screenshot("3-hidden-again"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

fn check_pointer(img: &screen::Image) -> Result<String, String> {
    let (x, y) = (SCREEN.0 / 2, SCREEN.1 / 2);
    // Six rows below the tip: the white outline just left of the hot spot,
    // the black inside to its right, and no dark arrow further left (only
    // a faint shadow, docs/u2-compositor-design.md §6).
    let edge = img.pixel(x - 1, y + 6);
    let inner = img.pixel(x + 2, y + 6);
    let left = img.pixel(x - 4, y + 6);
    let bright = |p: Option<[u8; 3]>| p.is_some_and(|p| p.iter().all(|&v| v >= 230));
    let dark = |p: Option<[u8; 3]>| p.is_some_and(|p| p.iter().all(|&v| v <= 25));
    if !bright(edge) || !dark(inner) || dark(left) {
        return Err(format!(
            "no pointer at ({x}, {y}): edge {edge:?}, inside {inner:?}, left of it {left:?}"
        ));
    }
    let t = title(img, "Files", true)?;
    Ok(format!(
        "pointer arrow at ({x}, {y}); `Files` focused at {t:?}"
    ))
}

fn check_click_focus(img: &screen::Image) -> Result<String, String> {
    let t = title(img, "Crash Test", true)?;
    let bar = menu_app(img, "Crash Test")?;
    no_title(img, "Files", true)?;
    Ok(format!(
        "after one click: `Crash Test` focused at {t:?} and named in the menu bar at {bar:?}; `Files` no longer focused"
    ))
}

fn check_drag(img: &screen::Image) -> Result<String, String> {
    let w = win(ASSISTANT_WIN);
    let moved = (
        (w.0 as i32 + DRAG.0) as usize,
        (w.1 as i32 + DRAG.1) as usize,
        w.2,
        w.3,
    );
    let want = title_pen(moved, "Assistant");
    let at = title(img, "Assistant", true)?;
    if at != want {
        return Err(format!(
            "`Assistant` title at {at:?}, expected {want:?} after the drag"
        ));
    }
    no_title(img, "Crash Test", true)?;
    Ok(format!(
        "`Assistant` title moved from {:?} to {at:?} and focused",
        title_pen(w, "Assistant")
    ))
}

// ---- U1 desktop geometry and text (docs/u1-design.md) -------------------

/// The screen the cases run on (QEMU's display, xtask/src/qemu.rs).
const SCREEN: (usize, usize) = (1920, 1080);
const FILES_WIN: usize = 0;
const CRASH_WIN: usize = 1;
const ASSISTANT_WIN: usize = 2;

/// Window `k`'s starting rectangle.
fn win(k: usize) -> (usize, usize, usize, usize) {
    zui::initial_windows(SCREEN.0, SCREEN.1)[k]
}

/// The centre of dock icon `k`.
fn dock_centre(k: usize) -> (usize, usize) {
    let (x, y) = zui::dock_icon(SCREEN.0, SCREEN.1, 4, k);
    (x + zui::DOCK_ICON / 2, y + zui::DOCK_ICON / 2)
}

/// A point in window `k`'s title bar, right of the traffic lights.
fn title_bar(k: usize) -> (usize, usize) {
    let w = win(k);
    (w.0 + w.2 * 3 / 4, w.1 + 10)
}

/// A point in window `k`'s content.
fn inside(k: usize) -> (usize, usize) {
    let w = win(k);
    (w.0 + w.2 / 2, w.1 + w.3 / 2)
}

/// The pointer, for relative moves: it starts at the screen's centre.
struct Ptr(i32, i32);

impl Ptr {
    fn start() -> Ptr {
        Ptr(SCREEN.0 as i32 / 2, SCREEN.1 as i32 / 2)
    }
    fn to(&mut self, (x, y): (usize, usize)) -> Action {
        let a = Action::Mouse(x as i32 - self.0, y as i32 - self.1);
        (self.0, self.1) = (x as i32, y as i32);
        a
    }
    fn by(&mut self, dx: i32, dy: i32) -> Action {
        self.0 += dx;
        self.1 += dy;
        Action::Mouse(dx, dy)
    }
}

/// Where window `w`'s title text starts: (x, baseline), as PeacockHandler
/// centres it.
fn title_pen(w: (usize, usize, usize, usize), text: &str) -> (usize, usize) {
    let tw = screen::ui_width(FaceId::Medium13, text);
    let lights_end = w.0 + zui::LIGHT_X + 2 * zui::LIGHT_GAP + 16;
    ((w.0 + w.2.saturating_sub(tw) / 2).max(lights_end), w.1 + 18)
}

fn ui(
    img: &screen::Image,
    face: FaceId,
    text: &str,
    fg: u32,
    bg: u32,
) -> Result<(usize, usize), String> {
    screen::find_ui(img, face, text, fg, bg).ok_or_else(|| format!("`{text}` not on screen"))
}

fn no_ui(img: &screen::Image, face: FaceId, text: &str, fg: u32, bg: u32) -> Result<(), String> {
    match screen::find_ui(img, face, text, fg, bg) {
        Some(at) => Err(format!(
            "wrong text `{text}` matched at {at:?}; the check is not discriminating"
        )),
        None => Ok(()),
    }
}

fn title_colours(focused: bool) -> (u32, u32) {
    if focused {
        (LIGHT.title_text, LIGHT.title)
    } else {
        (LIGHT.title_text_blurred, LIGHT.title_blurred)
    }
}

/// A window title (light appearance).
fn title(img: &screen::Image, text: &str, focused: bool) -> Result<(usize, usize), String> {
    let (fg, bg) = title_colours(focused);
    ui(img, FaceId::Medium13, text, fg, bg)
}

fn no_title(img: &screen::Image, text: &str, focused: bool) -> Result<(), String> {
    let (fg, bg) = title_colours(focused);
    no_ui(img, FaceId::Medium13, text, fg, bg)
}

/// A line of window content.
fn content(img: &screen::Image, text: &str) -> Result<(usize, usize), String> {
    ui(img, FaceId::Regular13, text, LIGHT.text, LIGHT.content)
}

fn no_content(img: &screen::Image, text: &str) -> Result<(), String> {
    no_ui(img, FaceId::Regular13, text, LIGHT.text, LIGHT.content)
}

fn selected_colours(focused: bool) -> (u32, u32) {
    if focused {
        (LIGHT.select_text, LIGHT.select)
    } else {
        (LIGHT.text, LIGHT.select_blurred)
    }
}

/// A selected line, in a focused window or another.
fn selected(img: &screen::Image, text: &str, focused: bool) -> Result<(usize, usize), String> {
    let (fg, bg) = selected_colours(focused);
    ui(img, FaceId::Regular13, text, fg, bg)
}

fn no_selected(img: &screen::Image, text: &str, focused: bool) -> Result<(), String> {
    let (fg, bg) = selected_colours(focused);
    no_ui(img, FaceId::Regular13, text, fg, bg)
}

/// The focused application's name in the menu bar.
fn menu_app(img: &screen::Image, text: &str) -> Result<(usize, usize), String> {
    ui(
        img,
        FaceId::SemiBold13,
        text,
        LIGHT.bar_text,
        zui::bar_color(&LIGHT),
    )
}

/// Other menu-bar text (the clock).
fn menu_text(img: &screen::Image, text: &str) -> Result<(usize, usize), String> {
    ui(
        img,
        FaceId::Regular13,
        text,
        LIGHT.bar_text,
        zui::bar_color(&LIGHT),
    )
}

/// A notification banner's body line.
fn banner_body(img: &screen::Image, text: &str) -> Result<(usize, usize), String> {
    ui(img, FaceId::Regular13, text, LIGHT.text, LIGHT.banner)
}

fn no_banner_body(img: &screen::Image, text: &str) -> Result<(), String> {
    no_ui(img, FaceId::Regular13, text, LIGHT.text, LIGHT.banner)
}

/// A dock icon's name, shown above it while the pointer is on it.
fn tooltip(img: &screen::Image, text: &str) -> Result<(usize, usize), String> {
    ui(
        img,
        FaceId::Regular13,
        text,
        LIGHT.tooltip_text,
        LIGHT.tooltip,
    )
}

fn no_tooltip(img: &screen::Image, text: &str) -> Result<(), String> {
    no_ui(
        img,
        FaceId::Regular13,
        text,
        LIGHT.tooltip_text,
        LIGHT.tooltip,
    )
}

/// The wallpaper's colour on screen row `y` (light appearance).
fn wallpaper(y: usize) -> [u8; 3] {
    let c = zoo_abi::font::blend(
        LIGHT.wall_top,
        LIGHT.wall_bottom,
        (y * 255 / (SCREEN.1 - 1)) as u8,
    );
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// The desktop on the virtio GPU: drawn (so flushed), with Crash Test
/// closed, and no pointer in the picture (the host draws the cursor).
fn check_gpu_desktop(img: &screen::Image) -> Result<String, String> {
    let home = content(img, "Home")?;
    no_title(img, "Crash Test", false)?;
    no_title(img, "Crash Test", true)?;
    Ok(format!(
        "Files content `Home` at {home:?}; Crash Test closed"
    ))
}

fn check_desktop_started(img: &screen::Image) -> Result<String, String> {
    let menu = menu_app(img, "Files")?;
    let t = title(img, "Files", true)?;
    content(img, "Home")?;
    let first = selected(img, "Documents/", true)?;
    content(img, "This window is a crash test.")?;
    title(img, "Assistant", false)?;
    no_selected(img, "Documentsx/", true)?;
    no_title(img, "Files", false)?;
    Ok(format!(
        "menu bar names `Files` at {menu:?}; `Files` focused (title at {t:?}) with `Documents/` selected at {first:?}; Crash Test and the Assistant shown; wrong text rejected"
    ))
}

fn check_file_details(img: &screen::Image) -> Result<String, String> {
    let name = content(img, "Home/Photos")?;
    let size = selected(img, "README.txt (34 bytes)", true)?;
    no_content(img, "Home/Music")?;
    Ok(format!(
        "3rd entry opened: `Home/Photos` at {name:?}, `README.txt (34 bytes)` selected at {size:?}; wrong folder rejected"
    ))
}

fn check_app_restart(img: &screen::Image) -> Result<String, String> {
    let body = banner_body(img, "It crashed and was restarted (1 of 3).")?;
    let t = title(img, "Crash Test", true)?;
    content(img, "This window is a crash test.")?;
    // Files kept its state through Crash Test's crash.
    content(img, "Home/Photos")?;
    no_banner_body(img, "It crashed and was restarted (2 of 3).")?;
    Ok(format!(
        "banner `It crashed and was restarted (1 of 3).` at {body:?}, focused `Crash Test` at {t:?}, Files details unchanged"
    ))
}

fn check_server_restart(img: &screen::Image) -> Result<String, String> {
    let menu = menu_app(img, "Files")?;
    let t = title(img, "Files", true)?;
    let first = selected(img, "Documents/", true)?;
    // A fresh window server: the old crash banner is gone.
    no_banner_body(img, "It crashed and was restarted (1 of 3).")?;
    Ok(format!(
        "fresh desktop after the window-server crash: menu bar `Files` at {menu:?}, `Files` focused at {t:?}, list reset at {first:?}"
    ))
}

fn check_console_back(img: &screen::Image) -> Result<String, String> {
    let ended = screen::find_row(img, "Desktop session ended.")
        .ok_or("no row shows `Desktop session ended.`")?;
    let row = screen::find_row(img, "zoo> echo back").ok_or("no row shows `zoo> echo back`")?;
    if !screen::row_matches(img, "back", row + 1) {
        return Err(format!("row {} does not show `back`", row + 1));
    }
    if screen::find_row(img, "zoo> echo bock").is_some() {
        return Err("a wrong string matched; the check is not discriminating".into());
    }
    if menu_app(img, "Files").is_ok() {
        return Err("the desktop menu bar is still on screen".into());
    }
    Ok(format!(
        "kernel console back: `Desktop session ended.` row {ended}, `zoo> echo back` row {row}, `back` row {}; no desktop left",
        row + 1
    ))
}

/// Key presses that type `text` (letters, digits, space, `-`, `/`, `.`),
/// then Return.
fn typed(text: &str) -> Vec<&'static str> {
    const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    text.chars()
        .map(|c| match c {
            'a'..='z' => KEYS[(c as u8 - b'a') as usize],
            'A'..='Z' => SHIFT_KEYS[(c as u8 - b'A') as usize],
            '0'..='9' => DIGITS[(c as u8 - b'0') as usize],
            ' ' => "spc",
            '-' => "minus",
            '/' => "slash",
            '.' => "dot",
            '%' => "shift+5",
            '=' => "equal",
            '_' => "shift+minus",
            '+' => "shift+equal",
            ',' => "comma",
            other => panic!("typed: no key for {other:?}"),
        })
        .chain(["ret"])
        .collect()
}

const SHIFT_KEYS: [&str; 26] = [
    "shift+a", "shift+b", "shift+c", "shift+d", "shift+e", "shift+f", "shift+g", "shift+h",
    "shift+i", "shift+j", "shift+k", "shift+l", "shift+m", "shift+n", "shift+o", "shift+p",
    "shift+q", "shift+r", "shift+s", "shift+t", "shift+u", "shift+v", "shift+w", "shift+x",
    "shift+y", "shift+z",
];

/// QEMU key names for `a`..`z`.
const KEYS: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];

/// Flips one byte inside the metadata of the newest generation on a data
/// disk (ZooFS 2, docs/linux10c-design.md): superblocks A and B are blocks
/// 0 and 1; the metadata's first run starts at the block named at byte 40.
/// Returns what it did.
fn corrupt_newest_snapshot(path: &std::path::Path) -> Result<String, String> {
    use std::io::{Read, Seek, SeekFrom, Write};
    const MAGIC: &[u8; 8] = b"ZOOFS002";
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut best: Option<(u64, u64, u64)> = None; // (generation, slot, metadata block)
    for slot in [0u64, 1] {
        let mut h = [0u8; 48];
        f.seek(SeekFrom::Start(slot * 4096))
            .map_err(|e| e.to_string())?;
        f.read_exact(&mut h).map_err(|e| e.to_string())?;
        if &h[..8] == MAGIC {
            let generation = u64::from_le_bytes(h[8..16].try_into().expect("8 bytes"));
            let meta = u64::from_le_bytes(h[40..48].try_into().expect("8 bytes"));
            if best.is_none_or(|(g, _, _)| generation > g) {
                best = Some((generation, slot, meta));
            }
        }
    }
    let (generation, slot, meta) = best.ok_or("no generation on the disk to corrupt")?;
    let at = meta * 4096 + 20;
    let mut b = [0u8; 1];
    f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    f.read_exact(&mut b).map_err(|e| e.to_string())?;
    b[0] ^= 0xFF;
    f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    f.write_all(&b).map_err(|e| e.to_string())?;
    Ok(format!(
        "corrupted generation {generation} (superblock {slot}, metadata at block {meta}): flipped byte {at}"
    ))
}

/// Every process line `guillemot-test`'s children print, in order
/// (c/linux/guillemot-test.c; pids are given out in sequence from 27).
fn guillemot_children() -> Vec<String> {
    let g = "guillemot-test";
    let mut lines = Vec::new();
    let mut child = |pid: u32, exec: bool, end: &str| {
        lines.push(format!("ZOO_OS PROC start pid={pid} name={g}"));
        let name = if exec {
            lines.push(format!("ZOO_OS PROC exec pid={pid} name=hello"));
            "hello"
        } else {
            g
        };
        lines.push(format!("ZOO_OS PROC exit pid={pid} name={name} {end}"));
    };
    // Exit code, memory, pipe; execvp; posix_spawn; a missing program;
    // SIGTERM; SIGPIPE; the 256 KiB pipe; the home-folder write.
    child(27, false, "code=7");
    child(28, false, "code=0");
    child(29, false, "code=0");
    child(30, true, "code=0");
    child(31, true, "code=0");
    child(32, false, "code=127");
    child(33, false, "signal=15");
    child(34, false, "signal=13");
    child(35, false, "code=0");
    child(36, false, "code=0");
    // Timings: 50 forks, 10 forks with 4 MiB, 50 spawns.
    for pid in 37..97 {
        child(pid, false, "code=0");
    }
    for pid in 97..147 {
        child(pid, true, "code=0");
    }
    lines
}

/// The process lines of `dovekie-test`'s children: 10 static then 10
/// dynamic `hello` starts (c/linux/dovekie-test.c).
fn dovekie_children() -> Vec<String> {
    let g = "dovekie-test";
    let mut lines = Vec::new();
    for (k, name) in (27..47).map(|pid| (pid, if pid < 37 { "hello" } else { "hello-dyn" })) {
        lines.push(format!("ZOO_OS PROC start pid={k} name={g}"));
        lines.push(format!("ZOO_OS PROC exec pid={k} name={name}"));
        lines.push(format!("ZOO_OS PROC exit pid={k} name={name} code=0"));
    }
    lines
}
