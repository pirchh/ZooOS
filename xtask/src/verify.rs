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

/// Wait this long for quiet serial output before a screenshot.
const QUIET: Duration = Duration::from_millis(1200);

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
    let mut fast_boot = expect(Mode::FastBoot, &fast_head);
    fast_boot
        .then_any_order(&[
            "ZOO_OS PROC exit pid=7 name=files code=0",
            "ZOO_OS PROC exit pid=8 name=crash-test code=0",
            "ZOO_OS PROC exit pid=9 name=assistant code=0",
            "ZOO_OS PROC exit pid=5 name=zookeeper code=0",
        ])
        .then(&[
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

    let cases = [
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
                    "ZOO_OS EXCEPTION page-fault access=read cause=not-present addr=0xffffc00000101000",
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
            expectation: desktop,
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
            expectation: redraw,
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "partial redraws: after pointer moves, a click and a drag, the screen equals a full repaint",
            interaction: Some(redraw_steps()),
            screen_checks: vec![("1-partial", check_partial), ("2-full", check_full_repaint)],
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
                        // From the centre into Crash Test (840, 200), click.
                        Action::Mouse(200, -200),
                        Action::Button(true),
                        Action::Button(false),
                        Action::WaitContains("ZOO_OS INFO usb mouse input".into()),
                        Action::WaitContains("PeacockHandler: click focuses Crash Test".into()),
                        Action::Settle(QUIET),
                        Action::Screenshot("1-click"),
                    ]);
                    steps
                },
            }),
            screen_checks: vec![("1-click", check_click_focus)],
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
            purpose: "a screen wider than 1920: the desktop uses its top-left 1920x1200 part instead of refusing it",
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
                    steps.extend([
                        // The Notes tile, then up into its window.
                        Action::Mouse(144, 308),
                        Action::Button(true),
                        Action::Button(false),
                        Action::WaitContains("PeacockHandler: opened Notes".into()),
                        Action::Mouse(0, -400),
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
                    steps.extend([
                        // Crash Test is (652, 48) 604x340: grab its corner
                        // area at (1250, 382) and drag by (-200, -100).
                        Action::Mouse(610, -18),
                        Action::Button(true),
                        Action::Mouse(-200, -100),
                        Action::Button(false),
                        Action::WaitContains(
                            "PeacockHandler: resized Crash Test to 404x240".into(),
                        ),
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
                    steps.extend([
                        // The Assistant's title bar at (900, 424).
                        Action::Mouse(260, 24),
                        Action::Button(true),
                        Action::Button(false),
                        Action::Button(true),
                        Action::Button(false),
                        Action::WaitContains("PeacockHandler: zoomed Assistant".into()),
                        Action::Settle(QUIET),
                        Action::Screenshot("1-zoomed"),
                        // Its title bar is now at the top: (900, 60).
                        Action::Mouse(0, -364),
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
                ("4-reopened", check_reopened),
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
        if c.name == "assistant-live" && qemu::accel() == "tcg" {
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

const DESKTOP_BG: u32 = 0x1E3A4C;
const BAR: u32 = 0xE8E8E8;
const BAR_TEXT: u32 = 0x202020;
const TITLE_FOCUSED: u32 = 0x3D7EBF;
const TITLE_FOCUSED_TEXT: u32 = 0xFFFFFF;
const TITLE_UNFOCUSED: u32 = 0x9AA5AE;
const TITLE_UNFOCUSED_TEXT: u32 = 0x202020;
const CONTENT: u32 = 0xFFFFFF;
const CONTENT_TEXT: u32 = 0x202020;
const SELECTED: u32 = 0xCFE3F7;
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
    let title = need(img, "Assistant", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    need(img, "You: search pancake", CONTENT_TEXT, CONTENT)?;
    // Two leading spaces: the broker's reply, not the Files window's
    // `home/user/Documents/...` entry.
    let found = need(
        img,
        "  Documents/recipes/pancakes.txt",
        CONTENT_TEXT,
        CONTENT,
    )?;
    need(img, "  would create Projects", CONTENT_TEXT, CONTENT)?;
    need(img, "  would create Projects/2026", CONTENT_TEXT, CONTENT)?;
    let ask = need(
        img,
        "Press y to create, n to leave it.",
        CONTENT_TEXT,
        CONTENT,
    )?;
    // Nothing is created before the confirmation.
    absent(img, "  created Projects", CONTENT_TEXT, CONTENT)?;
    Ok(format!(
        "`Assistant` focused at {title:?}; search result at {found:?}; plan shown and confirmation asked at {ask:?}; nothing created yet"
    ))
}

fn check_assistant_transcript(img: &screen::Image) -> Result<String, String> {
    let created = need(img, "  created Projects/2026", CONTENT_TEXT, CONTENT)?;
    need(img, "  removed Projects/2026", CONTENT_TEXT, CONTENT)?;
    let removed = need(img, "  removed Projects", CONTENT_TEXT, CONTENT)?;
    need(img, "Error: not found", CONTENT_TEXT, CONTENT)?;
    let escape = need(img, "Error: path escapes home", CONTENT_TEXT, CONTENT)?;
    let model = need(
        img,
        "The model is not installed; type a command instead.",
        CONTENT_TEXT,
        CONTENT,
    )?;
    absent(img, "  created Projects/2027", CONTENT_TEXT, CONTENT)?;
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
    let title = need(img, "Files", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    let sel = need(img, "Photos/", CONTENT_TEXT, SELECTED)?;
    absent(img, "Documents/", CONTENT_TEXT, SELECTED)?;
    let busy = need(img, "Thinking: ", CONTENT_TEXT, CONTENT)?;
    Ok(format!(
        "while the model works (status `Thinking:` at {busy:?}), Files took focus at {title:?} and its selection moved to the 3rd entry at {sel:?}"
    ))
}

fn check_cancelled(img: &screen::Image) -> Result<String, String> {
    let done = need(img, "Cancelled.", CONTENT_TEXT, CONTENT)?;
    let hint = need(img, HINT_TEXT, CONTENT_TEXT, CONTENT)?;
    absent(img, "Thinking: ", CONTENT_TEXT, CONTENT)?;
    Ok(format!(
        "`Cancelled.` at {done:?}, ready again at {hint:?}, no longer thinking"
    ))
}

fn check_live(img: &screen::Image) -> Result<String, String> {
    let s = need(
        img,
        "Assistant: search pancake recipe",
        CONTENT_TEXT,
        CONTENT,
    )?;
    need(
        img,
        "  Documents/recipes/pancakes.txt",
        CONTENT_TEXT,
        CONTENT,
    )?;
    let c = need(
        img,
        "Assistant: create Projects/2026",
        CONTENT_TEXT,
        CONTENT,
    )?;
    let made = need(img, "  created Projects/2026", CONTENT_TEXT, CONTENT)?;
    absent(img, "Assistant: list", CONTENT_TEXT, CONTENT)?;
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

/// Screen positions for a 1280x800 display (the QEMU default here): the
/// pointer starts at the centre; the Assistant's title bar starts at
/// (652, 412) and its title text at (660, 416).
const POINTER_START: (usize, usize) = (640, 400);
const ASSISTANT_TITLE_TEXT: (usize, usize) = (660, 416);
/// The drag: grab the Assistant title at (900, 424), move by (-300, -100).
const DRAG: (i32, i32) = (-300, -100);

fn mouse_steps() -> Interaction {
    Interaction {
        steps: vec![
            Action::WaitFor("ZOO_OS SHELL ready".into()),
            Action::Keys(vec!["d", "e", "s", "k", "t", "o", "p", "ret"]),
            Action::WaitFor("ZOO_OS PROC start pid=30 name=assistant".into()),
            Action::Settle(QUIET),
            Action::Screenshot("1-pointer"),
            // Into the Crash Test window (840, 200) and click.
            Action::Mouse(200, -200),
            Action::Button(true),
            Action::Button(false),
            Action::WaitContains("PeacockHandler: click focuses Crash Test".into()),
            Action::Settle(QUIET),
            Action::Screenshot("2-click"),
            // Onto the Assistant's title bar (900, 424), then drag it.
            Action::Mouse(60, 224),
            Action::Button(true),
            Action::Mouse(DRAG.0, DRAG.1),
            Action::Button(false),
            Action::WaitContains("PeacockHandler: moved Assistant to (352, 312)".into()),
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

fn redraw_steps() -> Interaction {
    let mut steps = start_desktop();
    steps.extend([
        // From the centre into Crash Test (840, 200) and click: focus
        // changes.
        Action::Mouse(200, -200),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: click focuses Crash Test".into()),
        // Onto the Files title bar (300, 60) and drag it by (100, 50).
        Action::Mouse(-540, -140),
        Action::Button(true),
        Action::Mouse(100, 50),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: moved Files to (124, 98)".into()),
        Action::Keys(vec!["tab"]),
    ]);
    // Focus changes and drags of large windows become full repaints, so
    // the pointer wanders last: each old position must be cleaned up by a
    // partial redraw (over the desktop, the windows and their borders).
    for _ in 0..8 {
        steps.push(Action::Mouse(45, 40));
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
fn pointer_sweep(steps: &mut Vec<Action>) {
    for dx in [12, -12] {
        for _ in 0..20 {
            steps.push(Action::Mouse(dx, 0));
        }
    }
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
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()));
    Interaction { steps }
}

fn check_partial(img: &screen::Image) -> Result<String, String> {
    // Clicking the Files title focused Files; Tab then moved on to Crash Test.
    let title = need(img, "Crash Test", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    let files = need(img, "Files", TITLE_UNFOCUSED_TEXT, TITLE_UNFOCUSED)?;
    if files != (132, 102) {
        return Err(format!(
            "`Files` title at {files:?}, expected (132, 102) after the drag"
        ));
    }
    Ok(format!(
        "`Crash Test` focused at {title:?}; `Files` dragged to {files:?}"
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

const MENU_BAR_H: usize = 24;

// ---- Large screens (docs/usb-boot.md) -----------------------------------

fn check_big_screen(img: &screen::Image) -> Result<String, String> {
    if img.width <= 1920 {
        return Err(format!(
            "the screen is {}x{}; this case needs one wider than 1920",
            img.width, img.height
        ));
    }
    let report = check_desktop_started(img)?;
    // Right of the desktop: painted black once.
    let outside = img.pixel(img.width - 2, 10);
    if outside != Some([0, 0, 0]) {
        return Err(format!(
            "outside the desktop at ({}, 10): {outside:?}",
            img.width - 2
        ));
    }
    Ok(format!(
        "{}x{} screen: {report}; the area right of x=1920 is black",
        img.width, img.height
    ))
}

// ---- D7 Files (docs/d7-design.md §3) -------------------------------------

fn check_preview(img: &screen::Image) -> Result<String, String> {
    let at = need(img, "Home/Documents/notes.txt", CONTENT_TEXT, CONTENT)?;
    let first = need(img, "Shopping: flour, eggs, milk.", CONTENT_TEXT, CONTENT)?;
    need(
        img,
        "Call the vet about Zara the zebra.",
        CONTENT_TEXT,
        CONTENT,
    )?;
    need(img, "Backspace: back to the folder", CONTENT_TEXT, CONTENT)?;
    absent(img, "Shopping: flour, eggs, milk.?", CONTENT_TEXT, CONTENT)?;
    Ok(format!(
        "preview of `Home/Documents/notes.txt` at {at:?}: its first line at {first:?}"
    ))
}

fn check_back_up(img: &screen::Image) -> Result<String, String> {
    let sel = need(img, "Photos/", CONTENT_TEXT, SELECTED)?;
    absent(img, "Documents/", CONTENT_TEXT, SELECTED)?;
    absent(img, "Home/Photos", CONTENT_TEXT, CONTENT)?;
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
        "{} {} {} {:02}:{:02} UTC",
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
        if let Some(at) = screen::find_text(img, &want, BAR_TEXT, BAR) {
            absent(img, &menu_clock_text(secs + 86_400), BAR_TEXT, BAR)?;
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
    assert_eq!(menu_clock_text(0), "Thu 1 Jan 00:00 UTC");
    assert_eq!(menu_clock_text(1_793_428_260), "Sat 31 Oct 06:31 UTC");
    assert_eq!(menu_clock_text(951_782_400), "Tue 29 Feb 00:00 UTC");
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
    steps.extend([
        // The Notes tile: centre (784, 708).
        Action::Mouse(144, 308),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: opened Notes".into()),
        // Up into the window, so the dock hides.
        Action::Mouse(0, -400),
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
    let title = need(img, "Notes", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    let head = need(img, "Documents/notes.txt (saved)", CONTENT_TEXT, CONTENT)?;
    let old = need(img, "Shopping: flour, eggs, milk.", CONTENT_TEXT, CONTENT)?;
    let typed = need(img, "Hello from Notes", CONTENT_TEXT, CONTENT)?;
    let second = need(img, "second line", CONTENT_TEXT, CONTENT)?;
    absent(img, "Hello from Notez", CONTENT_TEXT, CONTENT)?;
    // The seed file has CRLF line ends; a shown carriage return is `?`.
    absent(img, "Shopping: flour, eggs, milk.?", CONTENT_TEXT, CONTENT)?;
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
    let at = need(img, "aX|bc", CONTENT_TEXT, CONTENT)?;
    absent(img, "aXY", CONTENT_TEXT, CONTENT)?;
    absent(img, "abcXY", CONTENT_TEXT, CONTENT)?;
    need(img, "Documents/notes.txt (saved)", CONTENT_TEXT, CONTENT)?;
    Ok(format!(
        "`aX|bc` at {at:?}: X inserted two places back, Y deleted at the cursor, saved"
    ))
}

// ---- D8 resize case (docs/d8-design.md) -----------------------------------

const CLOSE_RED: [u8; 3] = [0xE0, 0x44, 0x3E];

fn check_resized(img: &screen::Image) -> Result<String, String> {
    let title = need(img, "Crash Test", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    need(img, "This window is a crash test.", CONTENT_TEXT, CONTENT)?;
    // The close button follows the right edge: now at x 1036-1047.
    if img.pixel(1042, 60) != Some(CLOSE_RED) {
        return Err(format!(
            "no close button at (1042, 60): {:?}",
            img.pixel(1042, 60)
        ));
    }
    if img.pixel(1242, 60) == Some(CLOSE_RED) {
        return Err("the close button is still at the old edge (1242, 60)".into());
    }
    // Below the new bottom edge (y 288) is the desktop again.
    let below = img.pixel(900, 300);
    if below != Some([0x1E, 0x3A, 0x4C]) {
        return Err(format!("below the resized window at (900, 300): {below:?}"));
    }
    Ok(format!(
        "`Crash Test` focused at {title:?}, now 404x240: close button at the new edge, desktop below it"
    ))
}

// ---- D9 zoom case (docs/d9-design.md) -------------------------------------

fn check_zoomed(img: &screen::Image) -> Result<String, String> {
    let at = need(img, "Assistant", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    if at != (32, 52) {
        return Err(format!(
            "`Assistant` title at {at:?}, expected (32, 52) when zoomed"
        ));
    }
    absent(img, "Files", TITLE_UNFOCUSED_TEXT, TITLE_UNFOCUSED)?;
    absent(img, "Crash Test", TITLE_UNFOCUSED_TEXT, TITLE_UNFOCUSED)?;
    // The close button at the zoomed window's right edge.
    if img.pixel(1242, 60) != Some(CLOSE_RED) {
        return Err("no close button at the zoomed right edge (1242, 60)".into());
    }
    Ok(format!(
        "`Assistant` zoomed: title at {at:?}, covering Files and Crash Test"
    ))
}

fn check_restored(img: &screen::Image) -> Result<String, String> {
    let at = need(img, "Assistant", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    if at != (660, 416) {
        return Err(format!(
            "`Assistant` title at {at:?}, expected (660, 416) after restoring"
        ));
    }
    let files = need(img, "Files", TITLE_UNFOCUSED_TEXT, TITLE_UNFOCUSED)?;
    Ok(format!(
        "`Assistant` restored to {at:?}; `Files` visible again at {files:?}"
    ))
}

// ---- D3 dock case (docs/d3-design.md §3) -------------------------------

/// Crash Test is closed (it ends normally and is not restarted), then
/// started again from the dock.
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

/// Dock geometry on the 1280x800 screen with four apps: panel at
/// (440, 676), 400x92; slot `i` starts at x = 448 + 96 i; tiles 48 px at
/// y = 684; names at y = 736; running dots (4 px) at (slot + 46, 756).
const DOCK_BG: u32 = 0xDDE3E8;
const DOCK_TEXT_RGB: [u8; 3] = [0x20, 0x20, 0x20];

fn dock_dot(img: &screen::Image, slot: usize) -> bool {
    img.pixel(448 + 96 * slot + 47, 757) == Some(DOCK_TEXT_RGB)
}

fn dock_steps() -> Interaction {
    let mut steps = start_desktop();
    steps.extend([
        // Pointer to (452, 700), inside the dock but clear of the tiles.
        Action::Mouse(-188, 300),
        Action::Settle(QUIET),
        Action::Screenshot("1-dock"),
        // Crash Test's close button: centre (1242, 60).
        Action::Mouse(790, -640),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: closed Crash Test".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-closed"),
        // The Crash Test tile: centre (592, 708).
        Action::Mouse(-650, 648),
        Action::Settle(QUIET),
        Action::Screenshot("3-dock-closed"),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: opened Crash Test".into()),
        // Away from the dock, so it hides.
        Action::Mouse(0, -308),
        Action::Settle(QUIET),
        Action::Screenshot("4-reopened"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

fn check_dock_shown(img: &screen::Image) -> Result<String, String> {
    let files = need(img, "Files", BAR_TEXT, DOCK_BG)?;
    let crash = need(img, "Crash Test", BAR_TEXT, DOCK_BG)?;
    let assistant = need(img, "Assistant", BAR_TEXT, DOCK_BG)?;
    let notes = need(img, "Notes", BAR_TEXT, DOCK_BG)?;
    if !(0..3).all(|i| dock_dot(img, i)) || dock_dot(img, 3) {
        return Err("running dots should mark the first three apps only".into());
    }
    Ok(format!(
        "dock tiles `Files` {files:?}, `Crash Test` {crash:?}, `Assistant` {assistant:?}, `Notes` {notes:?}; running dots under the first three, none under Notes"
    ))
}

fn check_closed(img: &screen::Image) -> Result<String, String> {
    absent(img, "Crash Test", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    absent(img, "Crash Test", TITLE_UNFOCUSED_TEXT, TITLE_UNFOCUSED)?;
    absent(img, "This window is a crash test.", CONTENT_TEXT, CONTENT)?;
    absent(img, "Files", BAR_TEXT, DOCK_BG)?;
    let files = need(img, "Files", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    Ok(format!(
        "Crash Test window gone, `Files` still focused at {files:?}, dock hidden"
    ))
}

fn check_dock_closed(img: &screen::Image) -> Result<String, String> {
    need(img, "Crash Test", BAR_TEXT, DOCK_BG)?;
    if !dock_dot(img, 0) || dock_dot(img, 1) || !dock_dot(img, 2) || dock_dot(img, 3) {
        return Err("dots should mark Files and the Assistant only".into());
    }
    Ok("dock: no running dot under Crash Test, dots under Files and Assistant".into())
}

fn check_reopened(img: &screen::Image) -> Result<String, String> {
    let title = need(img, "Crash Test", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    let body = need(img, "This window is a crash test.", CONTENT_TEXT, CONTENT)?;
    absent(img, "Files", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    absent(img, "Crash Test", BAR_TEXT, DOCK_BG)?;
    Ok(format!(
        "Crash Test reopened and focused: title {title:?}, text {body:?}; dock hidden"
    ))
}

const BLACK: [u8; 3] = [0, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];

fn check_pointer(img: &screen::Image) -> Result<String, String> {
    let (x, y) = POINTER_START;
    // The arrow's tip and left edge are black, its inside white (row 6,
    // columns 1-5), and nothing is drawn left of the tip.
    let edge = img.pixel(x, y + 6);
    let inside = img.pixel(x + 3, y + 6);
    let left = img.pixel(x - 2, y + 6);
    if edge != Some(BLACK) || inside != Some(WHITE) || left == Some(BLACK) {
        return Err(format!(
            "no pointer at {:?}: edge {edge:?}, inside {inside:?}, left of it {left:?}",
            POINTER_START
        ));
    }
    let title = need(img, "Files", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    Ok(format!(
        "pointer arrow at {:?}; `Files` focused at {title:?}",
        POINTER_START
    ))
}

fn check_click_focus(img: &screen::Image) -> Result<String, String> {
    let title = need(img, "Crash Test", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    let bar = need(img, "Crash Test", BAR_TEXT, BAR)?;
    absent(img, "Files", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    Ok(format!(
        "after one click: `Crash Test` focused at {title:?} and named in the menu bar at {bar:?}; `Files` no longer focused"
    ))
}

fn check_drag(img: &screen::Image) -> Result<String, String> {
    let want = (
        (ASSISTANT_TITLE_TEXT.0 as i32 + DRAG.0) as usize,
        (ASSISTANT_TITLE_TEXT.1 as i32 + DRAG.1) as usize,
    );
    let at = need(img, "Assistant", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    if at != want {
        return Err(format!(
            "`Assistant` title at {at:?}, expected {want:?} after the drag"
        ));
    }
    absent(img, "Crash Test", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    Ok(format!(
        "`Assistant` title moved from {ASSISTANT_TITLE_TEXT:?} to {at:?} and focused"
    ))
}

/// Finds `text` in colours `fg` on `bg`, or explains what is missing.
fn need(img: &screen::Image, text: &str, fg: u32, bg: u32) -> Result<(usize, usize), String> {
    screen::find_text(img, text, fg, bg).ok_or_else(|| format!("`{text}` not on screen"))
}

/// Asserts `text` is absent, so a check cannot pass by matching anything.
fn absent(img: &screen::Image, text: &str, fg: u32, bg: u32) -> Result<(), String> {
    match screen::find_text(img, text, fg, bg) {
        Some(at) => Err(format!(
            "wrong text `{text}` matched at {at:?}; the check is not discriminating"
        )),
        None => Ok(()),
    }
}

fn check_desktop_started(img: &screen::Image) -> Result<String, String> {
    let menu = need(img, "ZooOS", BAR_TEXT, BAR)?;
    let title = need(img, "Files", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    need(img, "Home", CONTENT_TEXT, CONTENT)?;
    let first = need(img, "Documents/", CONTENT_TEXT, SELECTED)?;
    need(img, "This window is a crash test.", CONTENT_TEXT, CONTENT)?;
    absent(img, "Documentsx/", CONTENT_TEXT, SELECTED)?;
    absent(img, "Files", BAR_TEXT, DESKTOP_BG)?;
    Ok(format!(
        "menu `ZooOS` at {menu:?}, focused title `Files` at {title:?}, home folder with `Documents/` selected at {first:?}, Crash Test window shown; wrong text rejected"
    ))
}

fn check_file_details(img: &screen::Image) -> Result<String, String> {
    let name = need(img, "Home/Photos", CONTENT_TEXT, CONTENT)?;
    let size = need(img, "README.txt (34 bytes)", CONTENT_TEXT, SELECTED)?;
    absent(img, "Home/Music", CONTENT_TEXT, CONTENT)?;
    Ok(format!(
        "3rd entry opened: `Home/Photos` at {name:?}, `README.txt (34 bytes)` selected at {size:?}; wrong folder rejected"
    ))
}

fn check_app_restart(img: &screen::Image) -> Result<String, String> {
    let status = need(img, "Crash Test crashed; restarted (1 of 3)", BAR_TEXT, BAR)?;
    let title = need(img, "Crash Test", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    need(img, "This window is a crash test.", CONTENT_TEXT, CONTENT)?;
    // Files kept its state through Crash Test's crash.
    need(img, "Home/Photos", CONTENT_TEXT, CONTENT)?;
    absent(img, "Crash Test crashed; restarted (2 of 3)", BAR_TEXT, BAR)?;
    Ok(format!(
        "status `Crash Test crashed; restarted (1 of 3)` at {status:?}, focused `Crash Test` at {title:?}, Files details unchanged"
    ))
}

fn check_server_restart(img: &screen::Image) -> Result<String, String> {
    let menu = need(img, "ZooOS", BAR_TEXT, BAR)?;
    let title = need(img, "Files", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    let first = need(img, "Documents/", CONTENT_TEXT, SELECTED)?;
    need(img, "Tab: next window", BAR_TEXT, BAR)?;
    // A fresh window server: the old crash notice is gone.
    absent(img, "Crash Test crashed; restarted (1 of 3)", BAR_TEXT, BAR)?;
    Ok(format!(
        "fresh desktop after the window-server crash: `ZooOS` at {menu:?}, `Files` focused at {title:?}, list reset at {first:?}"
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
    if screen::find_text(img, "ZooOS", BAR_TEXT, BAR).is_some() {
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

/// Flips one byte inside the payload of the newest valid-looking snapshot
/// slot on a data disk (docs/s5-design.md §4). Returns what it did.
fn corrupt_newest_snapshot(path: &std::path::Path) -> Result<String, String> {
    use std::io::{Read, Seek, SeekFrom, Write};
    const MAGIC: &[u8; 8] = b"ZOOFS001";
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let half = qemu::DATA_DISK_BYTES / 2;
    let mut best: Option<(u64, u64)> = None; // (generation, slot offset)
    for slot in [0, half] {
        let mut h = [0u8; 16];
        f.seek(SeekFrom::Start(slot)).map_err(|e| e.to_string())?;
        f.read_exact(&mut h).map_err(|e| e.to_string())?;
        if &h[..8] == MAGIC {
            let generation = u64::from_le_bytes(h[8..16].try_into().expect("8 bytes"));
            if best.is_none_or(|(g, _)| generation > g) {
                best = Some((generation, slot));
            }
        }
    }
    let (generation, slot) = best.ok_or("no snapshot on the disk to corrupt")?;
    let at = slot + 4096 + 20;
    let mut b = [0u8; 1];
    f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    f.read_exact(&mut b).map_err(|e| e.to_string())?;
    b[0] ^= 0xFF;
    f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    f.write_all(&b).map_err(|e| e.to_string())?;
    Ok(format!(
        "corrupted generation {generation} (slot at byte {slot}): flipped byte {at}"
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
