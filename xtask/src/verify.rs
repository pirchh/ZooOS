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
    /// `--cpus N`: processors for every case (docs/smp-design.md); `smp-`
    /// cases get at least 4.
    pub cpus: u8,
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
        "ZOO_OS PROC start pid=30 name=bella",
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
        "ZOO_OS PROC start pid=9 name=bella",
        "ZOO_OS PROC exit pid=6 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=6",
    ]);
    let fast_boot_desktop_exits = |m: Mode| {
        let mut e = expect(m, &fast_head);
        e.then_any_order(&[
            "ZOO_OS PROC exit pid=7 name=files code=0",
            "ZOO_OS PROC exit pid=8 name=crash-test code=0",
            "ZOO_OS PROC exit pid=9 name=bella code=0",
            "ZOO_OS PROC exit pid=5 name=zookeeper code=0",
        ]);
        e
    };
    // A3: the everyday system with the model built in. Bella starts the
    // file index (docs/vectors-design.md) and the model service at once to
    // warm it up (docs/stage2-design.md), then the broker on the first
    // request (docs/a3-design.md).
    let mut everyday_model = expect(Mode::FastBoot, &fast_head[..fast_head.len() - 2]);
    everyday_model.then(&[
        "ZOO_OS PROC start pid=10 name=magpie-handler",
        "ZOO_OS PROC start pid=11 name=elephant-handler",
        "ZOO_OS PROC start pid=12 name=otter-handler",
        "ZOO_OS PROC exit pid=6 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=6",
    ]);
    everyday_model.then_any_order(&[
        "ZOO_OS PROC exit pid=7 name=files code=0",
        "ZOO_OS PROC exit pid=8 name=crash-test code=0",
        "ZOO_OS PROC exit pid=9 name=bella code=0",
        // The file index stops when Bella has gone, even mid-index.
        "ZOO_OS PROC exit pid=10 name=magpie-handler code=0",
        "ZOO_OS PROC exit pid=11 name=elephant-handler code=0",
        "ZOO_OS PROC exit pid=12 name=otter-handler code=0",
        "ZOO_OS PROC exit pid=5 name=zookeeper code=0",
    ]);
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
        "ZOO_OS PROC start pid=30 name=bella",
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
    // A larger model than the default needs a larger machine.
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
            // The boot image only holds the default model: a larger one
            // does not fit its FAT partition. With `--bella` the Bella
            // cases boot the plain image and read the model from the model
            // disk instead, as `run` does (docs/summary-design.md); the
            // cases that check the boot image's model are skipped.
            let on_disk = !crate::infer::model_in_boot_image(root);
            let in_boot_image = if on_disk {
                image::build(root, tools, image::Mode::Normal)?
            } else {
                image::build_model_image(
                    root,
                    tools,
                    image::Mode::Normal,
                    &data,
                    None,
                    crate::infer::GUEST_PROMPT,
                    &r.output_ids,
                )?
            };
            Some((
                Some(in_boot_image),
                // The everyday system as `run` boots it: the plain
                // fast-boot image and the model disk (docs/stage2-design.md §3).
                (
                    image::build(root, tools, image::Mode::FastBoot)?,
                    image::model_disk(root)?,
                ),
                on_disk,
                // The chat model alone, for the cases that compare chat
                // models from the model disk.
                if on_disk {
                    Some(image::model_disk_chat(root)?)
                } else {
                    None
                },
            ))
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
    // Hearing (docs/hearing-design.md), when Moonshine is downloaded: all
    // recordings, and two with the first mislabelled (negative).
    let all = crate::hear::VOICES.len() * crate::hear::SENTENCES.len();
    let hear_image = image::build_hear_image(root, tools, Mode::Normal, false, all, false)?;
    let hear_wrong_image = image::build_hear_image(root, tools, Mode::Normal, true, 2, false)?;
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
            Action::WaitFor("ZOO_OS PROC start pid=9 name=bella".into()),
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
                    Action::WaitFor("ZOO_OS PROC start pid=9 name=bella".into()),
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
                    Action::WaitFor("ZOO_OS PROC start pid=9 name=bella".into()),
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
                    Action::WaitFor("ZOO_OS PROC start pid=9 name=bella".into()),
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
            name: "finder",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: notes_window_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "Finder-style Files and the Notes editor: a sidebar and icons; a sidebar click; a double-click opens a Rust file in Notes, coloured; Ctrl+S saves it; New Folder from a right-click",
            interaction: Some(finder_steps()),
            screen_checks: vec![
                ("1-sidebar", check_finder),
                ("2-code", check_code_colours),
                ("3-folder", check_new_folder),
            ],
        },
        Case {
            name: "bella-memory",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: bella_memory_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "the Bella learns: a request it could not answer, then the command typed for it, is remembered; the same request in other words is answered from memory, also after a desktop restart; an unrelated request is not",
            interaction: Some(bella_memory_steps()),
            screen_checks: vec![
                ("1-recalled", check_recalled),
                ("2-after-restart", check_recalled_again),
            ],
        },
        Case {
            name: "notes-large",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: notes_window_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "Notes keeps a file over 8 KiB whole (B1): Music/liner-notes.txt (9,230 bytes) opened from Files, edited and saved is 9,234 bytes, not cut to 8 KiB",
            interaction: Some(notes_large_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-any-case",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: any_case_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "the Bella's file tools take names in any letter case, as the model writes them: `list music` lists the Music folder; a name that matches nothing still fails",
            interaction: Some(bella_any_case_steps()),
            screen_checks: vec![("1-listed", check_any_case)],
        },
        Case {
            name: "memory-guard",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: memory_guard_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "the Bella's memory learns only right answers and replays only safe ones (B3, B4, B8): a planted `undo` is ignored; a search that found nothing is not learned; `create Invoices` is not recalled for Receipts",
            interaction: Some(memory_guard_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "home-refresh",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: home_refresh_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "the desktop keeps working when the filesystem service restarts (B5): BadgerHandler is restarted while the desktop runs; the session gets a fresh home folder, reloads the desktop, and Files lists Documents again",
            interaction: Some(home_refresh_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "broker-restart",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: rest_after_desktop(&normal, "kill otter-handler after 20"),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "Bella survives its file broker ending (docs/stage2-design.md §4): OtterHandler killed mid-session; the next request reports it; after the back-off a new broker answers",
            interaction: Some(broker_restart_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-access",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "broad access, confirm deletes (docs/access-design.md): info; read a file found by name; open a folder in Files; move and undo; trash asks, `n` leaves it, `y` moves it to the Trash; launch an app",
            interaction: Some(bella_access_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-windows",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "window actions typed to Bella (docs/window-actions-design.md): `maximize notes` opens Notes and fills the screen; `minimize notes` hides it in the dock; `close notes` quits it; a second `close notes` says it is not open",
            interaction: Some(bella_windows_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-tiling",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "tiling typed to Bella (docs/tiling-design.md): `split notes and bella` puts Notes on the left half and Bella on the right; `split files, notes and network` three columns; a bare `split` the two front windows",
            interaction: Some(bella_tiling_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-steps",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "requests of several steps (docs/multi-step-design.md): \"launch notes and files, then minimize files\" runs both in order; \"open notes and maximize it\" opens notes.txt, then \"it\" is Notes",
            interaction: Some(bella_steps_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-layout",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "layouts with try-and-confirm (docs/layouts-design.md): \"put these in rows\" stacks the three windows and asks; yes learns it; \"two on top and one on the bottom\" is arranged and asked, no, \"stack them\" corrects it and yes learns both; the learned phrasing is then recalled without a question",
            interaction: Some(bella_layout_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-one",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "Bella is one (docs/one-bella-design.md): `split bella and bella and files` and Ctrl+N on her window never open a second Bella; the owner's demo opened three, each loading a model",
            interaction: Some(bella_one_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "claim",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run lynx-test",
                    "ZOO_OS PROC start pid=26 name=lynx-test",
                    "ZOO_OS PROC start pid=27 name=lynx-test",
                    "ZOO_OS PROC exit pid=27 name=lynx-test code=0",
                    "ZOO_OS PROC exit pid=26 name=lynx-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "the CLAIM system call (docs/one-bella-design.md): LynxTest claims a name, a copy of it is refused the same name while it lives, and the name is released at exit",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run lynx-test")),
                    // INFO lines are not expectation markers: waited for here.
                    Action::WaitContains("ZOO_OS INFO claim name=lynx pid=26".into()),
                    Action::WaitContains(
                        "ZOO_OS INFO claim refused name=lynx pid=27 holder=26".into(),
                    ),
                    Action::WaitContains("LynxTest: a second claim of lynx was refused".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=lynx-test code=0".into()),
                    // Released as the holder is reaped, after its exit line.
                    Action::WaitContains("ZOO_OS INFO claim released name=lynx pid=26".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "bella-notes",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "the action registry (docs/registry-design.md): Notes' write, change and remove, all from etc/actions/notes.actions; `remove` asks first, y removes and n keeps; \"add bread to my notes\" in the registry's own words, without the model",
            interaction: Some(bella_notes_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-actions",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "app actions (docs/actions-design.md): `write buy milk` asks Notes (opened for it) through the window server to add a line and save; Notes' answer comes back to Bella",
            interaction: Some(bella_actions_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-events",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "the window server's events (docs/events-design.md): the user opens Notes from Files' Applications; Bella, subscribed, takes `maximize it` to mean Notes",
            interaction: Some(bella_events_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-facts",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "Bella remembers what she is told (docs/facts-design.md): facts kept on disk across a desktop restart; a question picks the matching fact; forgetting asks, `n` keeps, `y` forgets",
            interaction: Some(bella_facts_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-lookup",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "Bella answers from the user's files (docs/lookup-design.md): \"what do my notes say about the vet?\" finds the line in Documents/notes.txt; a question nothing answers finds nothing and asks no model",
            interaction: Some(bella_lookup_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-summary",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "Bella reads a file for a summary (docs/summary-design.md): \"summarise the pancake recipe\" finds the file by name and hands its text to the model service; a missing file is an error, not a summary",
            interaction: Some(bella_summary_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-context",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "Bella sees the screen (docs/context-design.md): after Documents/zoo.rs is opened in Notes, \"what am I working on?\" gets the window server's snapshot, naming Notes and the file",
            interaction: Some(bella_context_steps(false)),
            screen_checks: vec![],
        },
        Case {
            name: "bella-audit",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "Bella's audit trail (docs/audit-design.md): every broker request gets a log line; changes also get a dated line in .bella-audit, kept across a desktop restart; \"what did you do?\" lists them and not the reads",
            interaction: Some(bella_audit_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "dock-magnify",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "dock magnification: the icon under the pointer grows above the dock, its far neighbour less; it settles back when the pointer leaves",
            interaction: Some(dock_magnify_steps()),
            screen_checks: vec![
                ("1-magnified", check_magnified),
                ("2-settled", check_settled),
            ],
        },
        Case {
            name: "menu-bar",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "menus that open from the menu bar: Window lists the windows and brings one forward; sliding across the bar switches menus; View switches the appearance; Ctrl+M minimises",
            interaction: Some(menu_bar_steps()),
            screen_checks: vec![
                ("1-window-menu", check_window_menu),
                ("2-after", check_menu_after),
            ],
        },
        Case {
            name: "appearance-saved",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: apps_folder_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "the appearance is kept: switched to dark, the desktop restarted, it starts dark",
            interaction: Some(Interaction {
                steps: {
                    let mut steps = start_desktop();
                    steps.extend([
                        Action::Keys(vec!["f8"]),
                        Action::WaitContains("PeacockHandler: appearance saved".into()),
                        Action::Keys(vec!["esc"]),
                        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
                        Action::WaitFor("ZOO_OS PROC exit pid=26 name=zookeeper code=0".into()),
                        Action::Keys(typed("desktop")),
                        Action::WaitContains("[32:peacock-handler] PeacockHandler: appearance dark".into()),
                        Action::WaitFor("ZOO_OS PROC start pid=35 name=bella".into()),
                        Action::Settle(QUIET),
                        Action::Screenshot("1-dark-again"),
                        Action::Keys(vec!["esc"]),
                        Action::WaitFor("ZOO_OS DISPLAY released pid=32".into()),
                    ]);
                    steps
                },
            }),
            screen_checks: vec![("1-dark-again", check_dark_again)],
        },
        Case {
            name: "dock-bounce",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: notes_window_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "the launch bounce: Notes opened from the dock in slow motion (Shift); its icon is caught in the air, then the bounce ends",
            interaction: Some(dock_bounce_steps()),
            screen_checks: vec![("1-in-the-air", check_bounce)],
        },
        Case {
            name: "files-manage",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: mouse_expectation(&normal),
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "Files manages files: Rename (typed in place), Move to Trash, the Trash in the sidebar, Put Back",
            interaction: Some(files_manage_steps()),
            screen_checks: vec![("1-trash", check_trash)],
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
            name: "gpu-monitors",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "two monitors (docs/monitors-design.md): virtio-gpu with two scanouts showing one 3840x1080 desktop; the menu bar and dock on monitor 1; Bella maximizes Notes on monitor 2 and moves Files there",
            interaction: Some(gpu_monitors_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "gpu-monitors-model",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "the owner's example on three monitors (docs/monitors-design.md): \"hey bella put notes maximized on monitor 3 please\" fills monitor 3 with Notes; \"can you move files to the second screen\" moves Files there (needs --accel)",
            interaction: Some(gpu_monitors_model_steps()),
            screen_checks: vec![],
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
            name: "smp-boot",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&["ZOO_OS SHELL line=cpus"]);
                e
            },
            timeout: s.timeout,
            want: "pass",
            purpose: "the other processors (docs/smp-design.md, step 1): with 4 processors the MADT lists 4, the 3 others start through the trampoline, report online with their own GDT, TSS and local APIC, and park; the boot runs as before",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitSeen("ZOO_OS INFO cpu 1 online apic=".into()),
                    Action::WaitSeen("ZOO_OS INFO cpu 2 online apic=".into()),
                    Action::WaitSeen("ZOO_OS INFO cpu 3 online apic=".into()),
                    // 4, or more with `--cpus` (they run with at least 4).
                    Action::WaitSeen(format!("ZOO_OS INFO smp cpus={}", s.cpus.max(4))),
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    // The boot's programs ran on the other processors too
                    // (step 2).
                    Action::Keys(typed("cpus")),
                    Action::WaitNumbers(
                        "ZOO_OS INFO cpu 1 switches=".into(),
                        vec![("switches=", 1, u64::MAX)],
                    ),
                    Action::WaitNumbers(
                        "ZOO_OS INFO cpu 2 switches=".into(),
                        vec![("switches=", 1, u64::MAX)],
                    ),
                    Action::WaitNumbers(
                        "ZOO_OS INFO cpu 3 switches=".into(),
                        vec![("switches=", 1, u64::MAX)],
                    ),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "heap-flood",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run hare-test",
                    "ZOO_OS PROC start pid=26 name=hare-test",
                    // The three fillers; each exits when told to go.
                    "ZOO_OS PROC start pid=27 name=hare-test",
                    "ZOO_OS PROC start pid=28 name=hare-test",
                    "ZOO_OS PROC start pid=29 name=hare-test",
                    "ZOO_OS PROC exit pid=27 name=hare-test code=0",
                    "ZOO_OS PROC exit pid=28 name=hare-test code=0",
                    "ZOO_OS PROC exit pid=29 name=hare-test code=0",
                    "ZOO_OS PROC exit pid=26 name=hare-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "B13, the kernel heap (docs/heap-design.md): channel ends cannot be parked where no close reaches them; three programs filling the heap get NOMEM, the kernel lives, and the heap is usable once they exit",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run hare-test")),
                    Action::WaitContains("HareTest: an end over its own channel: refused".into()),
                    Action::WaitContains("HareTest: an end carrying a handle: refused".into()),
                    Action::WaitContains("fillers refused with NOMEM".into()),
                    Action::WaitContains("HareTest: kernel heap ok".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=hare-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "net-ping",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run snowgoose-test",
                    "ZOO_OS PROC start pid=26 name=snowgoose-test",
                    "ZOO_OS PROC exit pid=26 name=snowgoose-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "network: virtio-net, then DHCP, ARP and three pings of the gateway on QEMU's isolated user network",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run snowgoose-test")),
                    Action::WaitContains("net: DHCP offer 10.0.2.15 from 10.0.2.2".into()),
                    // The isolated network gives no router or DNS server.
                    Action::WaitContains(
                        "SnowGooseTest: address 10.0.2.15 mask 255.255.255.0 gateway 0.0.0.0".into(),
                    ),
                    Action::WaitContains(
                        "SnowGooseTest: no router given; using the DHCP server 10.0.2.2".into(),
                    ),
                    Action::WaitContains("SnowGooseTest: gateway 10.0.2.2 is at ".into()),
                    Action::WaitContains("SnowGooseTest: ping 10.0.2.2 seq 3: reply within ".into()),
                    Action::WaitContains("SnowGooseTest: network ok".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=snowgoose-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "net-open",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run snowgoose-test",
                    "ZOO_OS PROC start pid=26 name=snowgoose-test",
                    "ZOO_OS PROC exit pid=26 name=snowgoose-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "network, unrestricted: DHCP gives the router and DNS server too; ARP and pings of the router (QEMU answers them itself, so no internet is needed)",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run snowgoose-test")),
                    Action::WaitContains(
                        "SnowGooseTest: address 10.0.2.15 mask 255.255.255.0 gateway 10.0.2.2 dns 10.0.2.3"
                            .into(),
                    ),
                    Action::WaitContains("SnowGooseTest: ping 10.0.2.2 seq 3: reply within ".into()),
                    Action::WaitContains("SnowGooseTest: network ok".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=snowgoose-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "net-http",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run gyrfalcon-test",
                    "ZOO_OS PROC start pid=26 name=gyrfalcon-test",
                    "ZOO_OS PROC exit pid=26 name=gyrfalcon-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "TCP: a connection to a test server on the host (forwarded to 10.0.2.100:80 on the isolated network), an HTTP request, the whole answer, and a close",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run gyrfalcon-test")),
                    Action::WaitContains("GyrfalconTest: connected to 10.0.2.100:80".into()),
                    Action::WaitContains(format!(
                        "status `HTTP/1.0 200 OK`, body `{TEST_BODY}`"
                    )),
                    Action::WaitContains("GyrfalconTest: http ok".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=gyrfalcon-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "audio-tone",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run orca-test",
                    "ZOO_OS PROC start pid=26 name=orca-test",
                    "ZOO_OS PROC exit pid=26 name=orca-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 2,
            want: "pass",
            purpose: "sound (docs/audio-design.md): OrcaTest plays one second of 440 Hz through the virtio-sound driver; QEMU records it to a WAV file whose pitch and length the harness measures",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run orca-test")),
                    Action::WaitContains("ZOO_OS INFO sound stream started".into()),
                    Action::WaitContains("OrcaTest: played 1000 ms of 440 Hz".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=orca-test code=0".into()),
                    // The last 200 ms still play after the last write.
                    Action::Settle(QUIET),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "audio-silence",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: normal.clone(),
            timeout: s.timeout * 2,
            want: "audio-mismatch",
            purpose: "negative: the same machine playing nothing; the WAV check must reject it (no tone), so a pass of `audio-tone` means a tone was played",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Settle(QUIET),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "hear-snowyowl",
            mode: Mode::Normal,
            custom_image: hear_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run snowyowl-test",
                    "ZOO_OS PROC start pid=26 name=snowyowl-test",
                    "ZOO_OS PROC exit pid=26 name=snowyowl-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 6,
            want: "pass",
            purpose: "hearing (docs/hearing-design.md): SnowyOwlTest transcribes 8 recordings of the host's two voices with Moonshine Tiny in ZooOS; every transcript must be the sentence said",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run snowyowl-test")),
                    Action::WaitContains("SnowyOwlTest: Moonshine ready".into()),
                    Action::WaitContains(format!("SnowyOwlTest: {all} of {all} heard right")),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=snowyowl-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "hear-mislabelled",
            mode: Mode::Normal,
            custom_image: hear_wrong_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run snowyowl-test",
                    "ZOO_OS PROC start pid=26 name=snowyowl-test",
                    "ZOO_OS PROC exit pid=26 name=snowyowl-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 3,
            want: "unexpected-marker",
            purpose: "negative: the first recording listed with another sentence; SnowyOwlTest must report it misheard and exit 1, so a pass of `hear-snowyowl` means the transcripts were compared",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run snowyowl-test")),
                    Action::WaitContains("SnowyOwlTest: MISHEARD".into()),
                    Action::WaitContains("ZOO_OS PROC exit pid=26 name=snowyowl-test".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "mic-hear",
            mode: Mode::Normal,
            custom_image: hear_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run reindeer-test",
                    "ZOO_OS PROC start pid=26 name=reindeer-test",
                    "ZOO_OS PROC exit pid=26 name=reindeer-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "the microphone (docs/microphone-design.md): QEMU's virtio-sound input plays the host voice saying \"Hey Bella, open notes and maximize it.\" (SDL's disk audio driver); ReindeerTest records 6 s through AUDIO_READ and Moonshine must hear that sentence",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run reindeer-test")),
                    // The shell's prompt may share these lines.
                    Action::WaitContains("ZOO_OS INFO microphone started".into()),
                    Action::WaitContains("ZOO_OS INFO microphone attached pid=26".into()),
                    Action::WaitContains("ReindeerTest: recorded".into()),
                    Action::WaitContains("equals the source".into()),
                    Action::WaitContains("ReindeerTest: heard".into()),
                    Action::WaitFor("ZOO_OS PROC exit pid=26 name=reindeer-test code=0".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "mic-silence",
            mode: Mode::Normal,
            custom_image: hear_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run reindeer-test",
                    "ZOO_OS PROC start pid=26 name=reindeer-test",
                    "ZOO_OS PROC exit pid=26 name=reindeer-test code=0",
                ]);
                e
            },
            timeout: s.timeout * 3,
            want: "unexpected-marker",
            purpose: "negative: the same machine with a silent microphone; ReindeerTest must not hear the sentence and exit 1, so a pass of `mic-hear` means the words came through the microphone",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run reindeer-test")),
                    // The shell's prompt may share these lines.
                    Action::WaitContains("ZOO_OS INFO microphone started".into()),
                    Action::WaitContains("ZOO_OS INFO microphone attached pid=26".into()),
                    Action::WaitContains("ReindeerTest: recorded".into()),
                    Action::WaitContains("ZOO_OS PROC exit pid=26 name=reindeer-test".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "voice-bella",
            mode: Mode::Normal,
            custom_image: hear_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "\"hey Bella\" (docs/listening-design.md): the microphone plays \"Hey Bella, open notes and maximize it.\"; BatHandler finds the utterance, hears it and passes it to Bella, who opens Notes and maximizes it",
            interaction: Some(voice_steps(true)),
            screen_checks: vec![],
        },
        Case {
            name: "voice-pause",
            mode: Mode::Normal,
            custom_image: hear_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "a pause after her name (docs/listening-design.md §7): the microphone plays \"Hey Bella.\", a second of room noise, then \"Open notes and maximize it.\"; BatHandler takes the second utterance as the request and Bella carries it out",
            interaction: Some(voice_pause_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "voice-confirm",
            mode: Mode::Normal,
            custom_image: hear_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "a spoken yes (docs/registry-design.md): the microphone plays \"Hey Bella, remove milk.\", then \"Yes.\"; Bella asks before the delete, takes the spoken answer without her name, and Notes takes milk off the shopping line",
            interaction: Some(voice_confirm_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "voice-not-for-bella",
            mode: Mode::Normal,
            custom_image: hear_image.clone(),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "the \"hey Bella\" gate (docs/listening-design.md): the microphone plays \"Put notes on the left and files on the right.\" without her name; BatHandler hears it and Bella must not act on it",
            interaction: Some(voice_steps(false)),
            screen_checks: vec![],
        },
        Case {
            name: "net-bella-browse",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: network_app_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "Bella's `browse` (docs/actions-design.md): the Network app is opened for it, connects and fetches http://10.0.2.100/ from the harness's server; its answer comes back to Bella",
            interaction: Some(bella_browse_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "net-app",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: network_app_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "the Network app: opened from Files' Applications, it connects (DHCP, gateway) and fetches http://10.0.2.100/ from the harness's server, showing its text",
            interaction: Some(network_app_steps()),
            screen_checks: vec![("1-fetched", check_network_app)],
        },
        Case {
            name: "snowgoose-no-net",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: {
                let mut e = normal.clone();
                e.then(&[
                    "ZOO_OS SHELL line=run snowgoose-test",
                    "ZOO_OS PROC start pid=26 name=snowgoose-test",
                    "ZOO_OS PROC exit pid=26 name=snowgoose-test code=0",
                ]);
                e
            },
            timeout: s.timeout,
            // No network device: the name does not start with `net-`.
            want: "unexpected-marker",
            purpose: "negative: without a network device SnowGooseTest must fail (exit 2), not pass",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS SHELL ready".into()),
                    Action::Keys(typed("run snowgoose-test")),
                    Action::WaitContains("SnowGooseTest: no network device".into()),
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
                        // The Bella's title bar.
                        p.to(title_bar(ASSISTANT_WIN)),
                        Action::Button(true),
                        Action::Button(false),
                        Action::Button(true),
                        Action::Button(false),
                        Action::WaitContains("PeacockHandler: zoomed Bella".into()),
                        Action::Settle(QUIET),
                        Action::Screenshot("1-zoomed"),
                        // Its title bar is now at the top.
                        p.to((z.0 + z.2 / 2, z.1 + 10)),
                        Action::Button(true),
                        Action::Button(false),
                        Action::Button(true),
                        Action::Button(false),
                        Action::WaitContains("PeacockHandler: restored Bella".into()),
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
            name: "bella",
            mode: Mode::Normal,
            custom_image: None,
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: bella_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "Bella window: typed commands through OtterHandler, plan + confirm before create, undo, escape refused, model absent reported",
            interaction: Some(bella_steps()),
            screen_checks: vec![
                ("1-plan", check_bella_plan),
                ("2-transcript", check_bella_transcript),
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
            name: "bella-model",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: bella_model_expectation(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "the model works on a request while the desktop stays usable; Backspace cancels it",
            interaction: Some(bella_model_steps()),
            screen_checks: vec![
                ("1-busy", check_busy_desktop),
                ("2-cancelled", check_cancelled),
            ],
        },
        Case {
            name: "model-restart",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: rest_after_desktop(&normal, "kill elephant-handler after 20"),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "Bella survives its model service ending (docs/stage2-design.md §4): ElephantHandler killed after its warm-up; Bella starts it again, it warms up again, and a request is answered (needs --accel)",
            interaction: Some(model_restart_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-talk",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: bella_model_expectation(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "Bella answers in words (docs/talk-design.md): small talk without a command; a request no command fits; and a sentence starting with `search` goes to the model, not run literally (needs --accel)",
            interaction: Some(bella_talk_steps()),
            screen_checks: vec![("1-talk", check_talk)],
        },
        Case {
            name: "bella-convo",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: bella_model_expectation(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "Bella keeps the conversation (docs/talk-design.md §6): a command's result is noted; \"is that it?\" is sent as a follow-up with the last exchange and answered in words, not as `list .` (needs --accel)",
            interaction: Some(bella_convo_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-facts-answer",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: bella_model_expectation(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "Bella answers from what she was told (docs/facts-design.md): told the dentist is on Friday, asked when the dentist is, her reply says Friday (needs --accel)",
            interaction: Some(bella_facts_answer_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-lookup-answer",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: bella_model_expectation(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "Bella answers from the user's files (docs/lookup-design.md): asked what the notes say about the vet, her answer names Zara, which only the file holds (needs --accel)",
            interaction: Some(bella_lookup_answer_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "usb-model",
            mode: Mode::FastBoot,
            // Built when the case runs: 1.7 GB (docs/usb-model-design.md).
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: everyday_model.clone(),
            timeout: s.timeout * 6,
            want: "pass",
            purpose: "the USB stick's image (docs/usb-model-design.md): the everyday system with Qwen3 and the embedding model in its boot image and no model disk, as on a real PC; both models are mapped from the boot image and Bella finds the pancake recipe (needs --accel)",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS PROC start pid=9 name=bella".into()),
                    Action::WaitContains(
                        "MagpieHandler: the embedding model is in the boot image".into(),
                    ),
                    Action::WaitContains("ElephantHandler: service ready (model mapped".into()),
                    Action::WaitContains("ElephantHandler: warmed up: system prompt".into()),
                    Action::Settle(QUIET),
                    // Files -> Crash Test -> Bella.
                    Action::Keys(vec!["tab", "tab"]),
                    Action::Settle(QUIET),
                    Action::Keys(typed("Find my pancake recipe")),
                    Action::WaitContains("ElephantHandler: chose search `pancake".into()),
                    Action::WaitContains("Bella: answered in ".into()),
                    Action::Settle(QUIET),
                    Action::NeverSeen("from the model disk".into()),
                    Action::Keys(vec!["esc"]),
                    Action::WaitFor("ZOO_OS DISPLAY released pid=6".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "smp-keys",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 6,
            want: "pass",
            purpose: "keys typed while the models load (docs/smp-design.md): with 8 processors, a 53-character request typed into Bella while the chat and embedding models are read from the model disk reaches the model service whole (needs --accel and the model disk)",
            interaction: Some(Interaction {
                steps: {
                    let mut steps = start_desktop();
                    // Files -> Crash Test -> Bella, at once: the models are
                    // still being read.
                    steps.push(Action::Keys(vec!["tab", "tab"]));
                    steps.push(Action::Keys(typed(KEYS_UNDER_LOAD)));
                    steps.push(Action::WaitContains(
                        "ElephantHandler: read the model from the model disk".into(),
                    ));
                    steps.push(Action::WaitContains(format!(
                        "ElephantHandler: `{KEYS_UNDER_LOAD}`"
                    )));
                    steps.push(Action::Settle(QUIET));
                    steps
                },
            }),
            screen_checks: vec![],
        },
        Case {
            name: "bella-vectors",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 12,
            want: "pass",
            purpose: "Bella's file index by meaning (docs/vectors-design.md): MagpieHandler embeds the home folder's lines; \"the animal doctor\" finds the vet line, which shares no word with it, and the answer names Zara; after a desktop restart the index is read back, not rebuilt (needs --accel and the embedding model)",
            interaction: Some(bella_vectors_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-summary-answer",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: bella_model_expectation(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "Bella summarises a file (docs/summary-design.md): Documents/notes.txt, read through the broker, summarised by the model in a call of its own; the summary names what only the file holds (needs --accel)",
            interaction: Some(bella_summary_answer_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-save",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "Bella saves a summary (docs/save-design.md): \"save that\" writes Documents/notes-summary.txt beside the file, a second save refuses to replace it, Notes opens it, and the audit trail lists the write (needs --accel)",
            interaction: Some(bella_save_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-windows-model",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "a window action in words (docs/window-actions-design.md): \"open notes and maximize it on my screen\" makes the model choose `maximize Notes`; Notes opens and fills the screen (needs --accel)",
            interaction: Some(bella_windows_model_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-tiling-model",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "tiling in words (docs/tiling-design.md): \"put notes on the left and the assistant on the right\" makes the model choose `split` and Bella read `Notes and Bella` from the request (needs --accel)",
            interaction: Some(bella_tiling_model_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-steps-model",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "several steps in words (docs/multi-step-design.md): \"hey bella open notes and files, then put them side by side\": the model chooses each step; \"them\" is Notes and Files (needs --accel)",
            interaction: Some(bella_steps_model_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-actions-model",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "an app action in words (docs/actions-design.md): \"hey bella take a note that the dentist is on friday\" makes the model choose `write`, and Notes writes the request's own text (needs --accel)",
            interaction: Some(bella_actions_model_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-context-answer",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: desktop_then_rest(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "Bella answers from the screen (docs/context-design.md): asked what the user is working on, her reply names zoo.rs (needs --accel)",
            interaction: Some(bella_context_steps(true)),
            screen_checks: vec![],
        },
        Case {
            name: "responsiveness",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: warm_only_expectation(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "responsiveness while the model runs (docs/stage1-design.md): the window server's input wait for pointer moves, idle and then during the model's set-up (needs --accel)",
            interaction: Some(responsiveness_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "bella-live",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: bella_model_expectation(&normal),
            timeout: s.timeout * 10,
            want: "pass",
            purpose: "requests in words: the model picks the tool, the broker runs it (needs --accel; too slow under TCG)",
            interaction: Some(bella_live_steps()),
            screen_checks: vec![("1-live", check_live)],
        },
        Case {
            name: "everyday-model",
            mode: Mode::FastBoot,
            custom_image: model_image.as_ref().map(|m| m.1.0.clone()),
            disk: Disk::Fresh,
            usb: Usb::None,
            expectation: everyday_model,
            timeout: s.timeout * 4,
            want: "pass",
            purpose: "the everyday system as `run` boots it: the fast-boot image and the model disk; the model is read from the disk, and Bella answers a request in words",
            interaction: Some(Interaction {
                steps: vec![
                    Action::WaitFor("ZOO_OS PROC start pid=9 name=bella".into()),
                    // The model comes from the model disk, not the boot
                    // image (docs/stage2-design.md §3).
                    Action::WaitContains(
                        "ElephantHandler: read the model from the model disk".into(),
                    ),
                    Action::Settle(QUIET),
                    // Files -> Crash Test -> Bella.
                    Action::Keys(vec!["tab", "tab"]),
                    Action::Settle(QUIET),
                    Action::Keys(typed("Find my pancake recipe")),
                    // `pancake recipe` (0.5B) or `pancake` (Qwen3): both find
                    // the recipe (assist-check accepts both).
                    Action::WaitContains("ElephantHandler: chose search `pancake".into()),
                    // Stage 1 timing lines (docs/stage1-design.md).
                    Action::WaitContains("ElephantHandler: timing: setup ".into()),
                    Action::WaitContains("Bella: answered in ".into()),
                    Action::Settle(QUIET),
                    Action::Keys(vec!["esc"]),
                    Action::WaitFor("ZOO_OS DISPLAY released pid=6".into()),
                ],
            }),
            screen_checks: vec![],
        },
        Case {
            name: "inference",
            mode: Mode::Normal,
            custom_image: model_image.as_ref().and_then(|m| m.0.clone()),
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

    let model_on_disk = model_image.as_ref().is_some_and(|m| m.2);
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
        let model_case = matches!(
            c.name,
            "inference"
                | "bella-model"
                | "bella-live"
                | "everyday-model"
                | "responsiveness"
                | "model-restart"
                | "bella-talk"
                | "bella-convo"
                | "bella-facts-answer"
                | "bella-context-answer"
                | "bella-windows-model"
                | "bella-tiling-model"
                | "bella-steps-model"
                | "gpu-monitors-model"
                | "bella-actions-model"
                | "bella-summary-answer"
                | "bella-lookup-answer"
                | "bella-vectors"
                | "smp-keys"
                | "usb-model"
                | "bella-save"
        );
        if model_case && model_on_disk && matches!(c.name, "inference" | "bella-model") {
            let line = format!(
                "SKIP  {:<22} not run: checks the model in the boot image (default model only)\n",
                c.name
            );
            print!("{line}");
            report.push_str(&line);
            continue;
        }
        if model_case && c.custom_image.is_none() {
            let line = format!(
                "SKIP  {:<22} not run: model unavailable (--skip-model)\n",
                c.name
            );
            print!("{line}");
            report.push_str(&line);
            continue;
        }
        if (c.name.starts_with("hear-")
            || c.name.starts_with("mic-")
            || c.name.starts_with("voice-"))
            && c.custom_image.is_none()
        {
            let line = format!(
                "SKIP  {:<22} not run: no hearing model or host voice (tools/models/README.md, \"Voice\")
",
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
        // Under an accelerator a triple fault stops the guest instead of
        // resetting it, so QEMU never exits early: the case needs TCG.
        if c.name == "negative-triple-fault" && qemu::accel() != "tcg" {
            let line = format!(
                "SKIP  {:<22} not run: a triple fault ends QEMU early only under TCG
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
        if matches!(
            c.name,
            "bella-live"
                | "everyday-model"
                | "responsiveness"
                | "model-restart"
                | "bella-talk"
                | "bella-convo"
                | "bella-facts-answer"
                | "bella-context-answer"
                | "bella-windows-model"
                | "bella-tiling-model"
                | "bella-steps-model"
                | "gpu-monitors-model"
                | "bella-actions-model"
                | "bella-summary-answer"
                | "bella-lookup-answer"
                | "bella-vectors"
                | "smp-keys"
                | "usb-model"
                | "bella-save"
                | "wine-home-1"
                | "wine-home-2"
        ) && qemu::accel() == "tcg"
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
        // The USB stick's image is 1.7 GB: built only when its case runs.
        let usb_img;
        let img = if c.name == "usb-model" {
            usb_img = image::build_usb_image(root, tools)?;
            &usb_img
        } else {
            img
        };
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
        // A model too large for the boot image needs 4 GiB beside the
        // desktop; every other case keeps 2 (timing-sensitive desktop cases
        // were measured at 2).
        let big_model = model_on_disk
            && (model_case || matches!(c.name, "bella-vectors" | "bella-learn" | "bella-save"));
        qemu::set_memory_gib(
            if big_model || matches!(c.name, "everyday-model" | "usb-model") {
                4
            } else {
                2
            },
        );
        // The other processors (docs/smp-design.md).
        qemu::set_cpus(if c.name == "smp-keys" {
            s.cpus.max(8)
        } else if c.name.starts_with("smp-") {
            s.cpus.max(4)
        } else {
            s.cpus
        });
        // The everyday system's model comes on the model disk.
        qemu::set_model_disk(if c.name == "usb-model" {
            // A real PC has none.
            None
        } else if matches!(c.name, "everyday-model" | "bella-vectors" | "smp-keys") {
            model_image.as_ref().map(|m| m.1.1.clone())
        } else if model_case && model_on_disk {
            model_image.as_ref().and_then(|m| m.3.clone())
        } else {
            None
        });
        // `net-` cases get an isolated network, `net-open` the open one
        // (docs/n1-design.md).
        qemu::set_net(if c.name == "net-open" {
            qemu::Net::Open
        } else if c.name.starts_with("net-") {
            qemu::Net::Isolated
        } else {
            qemu::Net::None
        });
        // `gpu-` cases run on `virtio-vga`; the rest on plain VGA, the
        // real-hardware path (docs/gpu1-design.md).
        qemu::set_display(if c.name.starts_with("gpu-") {
            qemu::Display::VirtioVga
        } else {
            qemu::Display::Vga
        });
        // `gpu-monitors` cases: two monitors (docs/monitors-design.md).
        qemu::set_monitors(if c.name == "gpu-monitors-model" {
            3
        } else if c.name.starts_with("gpu-monitors") {
            2
        } else {
            1
        });
        // `gpu-hidpi` cases: a 4K screen, drawn at 2x (docs/u2b-design.md).
        qemu::set_display_size(if c.name.starts_with("gpu-hidpi") {
            Some((3840, 2160))
        } else if c.name == "gpu-1440" {
            Some((2560, 1440))
        } else {
            None
        });
        // `net-http`: a small HTTP server on the host, which the guest
        // reaches at 10.0.2.100:80 (docs/n2-design.md).
        let server = if matches!(c.name, "net-http" | "net-app" | "net-bella-browse") {
            Some(TestServer::start()?)
        } else {
            None
        };
        qemu::set_guestfwd(server.as_ref().map(|s| s.port));
        // `audio-` cases: a sound device recorded to a WAV file.
        let wav = root
            .join("build")
            .join("runs")
            .join(c.name)
            .join("audio.wav");
        if c.name.starts_with("audio-") {
            let _ = std::fs::remove_file(&wav);
            if let Some(dir) = wav.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            qemu::set_audio(Some(wav.clone()));
        } else {
            qemu::set_audio(None);
        }
        // `mic-` cases: a microphone that plays a known recording
        // (docs/microphone-design.md).
        if c.name.starts_with("mic-") || c.name.starts_with("voice-") {
            let sentence = if c.name == "voice-pause" {
                "pause"
            } else if c.name == "voice-confirm" {
                "confirm"
            } else if c.name == "voice-not-for-bella" {
                "split"
            } else {
                "open-notes"
            };
            qemu::set_mic(Some(crate::hear::mic_input(
                root,
                sentence,
                c.name == "mic-silence",
                // The voice cases hear a noisy room, as `run --mic` does.
                c.name.starts_with("voice-"),
            )?));
        } else {
            qemu::set_mic(None);
        }
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
        );
        qemu::set_guestfwd(None);
        if let Some(server) = server {
            let served = server.stop();
            println!("[verify] test server answered {served} request(s)");
        }
        let r = r?;
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
        // The tone in the recording (docs/audio-design.md).
        if kind == "pass" && c.name.starts_with("audio-") {
            match check_tone(&wav, 440.0) {
                Ok(summary) => detail = format!("{detail}\n      audio: {summary}"),
                Err(why) => {
                    kind = "audio-mismatch".into();
                    detail = format!("audio: {why} ({})", wav.display());
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
        "ZOO_OS PROC start pid=30 name=bella",
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
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC start pid=32 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=32 name=peacock-handler",
        "ZOO_OS PROC start pid=33 name=files",
        "ZOO_OS PROC start pid=34 name=crash-test",
        "ZOO_OS PROC start pid=35 name=bella",
    ]);
    // 5. Escape ends the session cleanly; the kernel shell is back.
    e.then(&[
        "ZOO_OS PROC exit pid=32 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=32",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=33 name=files code=0",
        "ZOO_OS PROC exit pid=34 name=crash-test code=0",
        "ZOO_OS PROC exit pid=35 name=bella code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e.then(&["ZOO_OS SHELL line=echo back"]);
    e.clone()
}

/// The desktop with the Bella used through typed commands; the normal
/// image has no model, so a request in words reaches ElephantHandler, which
/// reports that (docs/m5c2-design.md §5).
fn bella_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=bella",
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
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC exit pid=31 name=otter-handler code=0",
        "ZOO_OS PROC exit pid=32 name=elephant-handler code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e.clone()
}

fn bella_steps() -> Interaction {
    let mut steps = vec![
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(typed("desktop")),
        Action::WaitFor("ZOO_OS PROC start pid=30 name=bella".into()),
        Action::Settle(QUIET),
        // Files -> Crash Test -> Bella.
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

fn check_bella_plan(img: &screen::Image) -> Result<String, String> {
    let title = title(img, "Bella", true)?;
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
        "`Bella` focused at {title:?}; search result at {found:?}; plan shown and confirmation asked at {ask:?}; nothing created yet"
    ))
}

fn check_bella_transcript(img: &screen::Image) -> Result<String, String> {
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
/// The desktop with the model, where Bella is asked nothing: only the
/// model service starts, to warm up (docs/stage2-design.md).
fn warm_only_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=bella",
        "ZOO_OS PROC start pid=31 name=elephant-handler",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC exit pid=31 name=elephant-handler code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

fn bella_model_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=bella",
        // The model service at once, to warm up (docs/stage2-design.md); the
        // broker on the first request (Bella asks its memory first).
        "ZOO_OS PROC start pid=31 name=elephant-handler",
        "ZOO_OS PROC start pid=32 name=otter-handler",
    ]);
    e.then(&[
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    let exits = vec![
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC exit pid=31 name=elephant-handler code=0",
        "ZOO_OS PROC exit pid=32 name=otter-handler code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ];
    e.then_any_order(&exits);
    e.clone()
}

fn open_bella() -> Vec<Action> {
    vec![
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(typed("desktop")),
        Action::WaitFor("ZOO_OS PROC start pid=30 name=bella".into()),
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

fn bella_model_steps() -> Interaction {
    let mut steps = open_bella();
    steps.extend([
        Action::Keys(typed("find my pancake recipe")),
        Action::WaitContains("ElephantHandler: service ready (model mapped".into()),
        Action::Settle(QUIET),
        // While the model computes: move to Files and down two entries.
        Action::Keys(vec!["tab", "down", "down"]),
        Action::Settle(QUIET),
        Action::Screenshot("1-busy"),
        // Back to the Bella (Files -> Crash Test -> Bella) and cancel.
        Action::Keys(vec!["tab", "tab", "backspace"]),
        Action::WaitFor("[31:elephant-handler] ElephantHandler: request cancelled".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-cancelled"),
    ]);
    leave_desktop(&mut steps);
    Interaction { steps }
}

/// Pointer moves for the responsiveness case: a square, `rounds` times.
fn wiggle(p: &mut Ptr, rounds: usize) -> Vec<Action> {
    let mut v = Vec::new();
    for _ in 0..rounds {
        for (dx, dy) in [(40, 0), (0, 40), (-40, 0), (0, -40)] {
            v.push(p.by(dx, dy));
        }
    }
    v
}

fn responsiveness_steps() -> Interaction {
    let mut p = Ptr::start();
    // F11 logs the window server's frame and input-wait figures and starts
    // them over. The model warms up in the background as soon as Bella
    // opens (docs/stage2-design.md): first, pointer moves during that warm-up;
    // then, once it is over, the same moves with the desktop idle.
    let mut steps = vec![
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(typed("desktop")),
        Action::WaitContains("ElephantHandler: service ready (model mapped".into()),
        Action::Keys(vec!["f11"]),
        Action::WaitContains("PeacockHandler: input wait".into()),
    ];
    steps.extend(wiggle(&mut p, 10));
    steps.push(Action::Keys(vec!["f11"]));
    // While the model runs, the mean wait must stay under 15% of a tick
    // (1.5 ms); it was 47% before scheduler priorities (docs/stage2-design.md).
    steps.push(Action::WaitNumbers(
        "PeacockHandler: input wait".into(),
        vec![("input wait", 20, 1000), ("tick: mean", 0, 150)],
    ));
    // The warm-up ended only after those figures: the model was busy
    // throughout.
    steps.push(Action::WaitContains("ElephantHandler: warmed up".into()));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["f11"]));
    steps.push(Action::WaitContains("PeacockHandler: input wait".into()));
    steps.extend(wiggle(&mut p, 10));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["f11"]));
    steps.push(Action::WaitContains("PeacockHandler: input wait".into()));
    leave_desktop(&mut steps);
    Interaction { steps }
}

/// A `kill … after …` shell line, then the desktop; after that, process
/// numbers depend on timing.
fn rest_after_desktop(normal: &Expectation, kill: &str) -> Expectation {
    let mut e = normal.clone();
    let line = format!("ZOO_OS SHELL line={kill}");
    e.then(&[
        line.as_str(),
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
    ]);
    e.then_rest();
    e
}

fn broker_restart_steps() -> Interaction {
    let mut steps = vec![
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(typed("kill otter-handler after 20")),
        Action::Keys(typed("desktop")),
        Action::WaitFor("ZOO_OS PROC start pid=30 name=bella".into()),
        Action::Settle(QUIET),
        // Files -> Crash Test -> Bella.
        Action::Keys(vec!["tab", "tab"]),
        Action::Settle(QUIET),
        // Starts the broker.
        Action::Keys(typed("list music")),
        Action::WaitContains("OtterHandler: `music` is `Music`".into()),
        Action::WaitContains("name=otter-handler by=0".into()),
        Action::Settle(QUIET),
        // The ended broker is noticed on the next request ...
        Action::Keys(typed("list music")),
        Action::WaitContains("Bella: the file broker stopped; a new one in 500 ms".into()),
        Action::Settle(QUIET),
    ];
    // ... and after the back-off a new one answers.
    steps.push(Action::Keys(typed("list music")));
    steps.push(Action::WaitContains(
        "OtterHandler: `music` is `Music`".into(),
    ));
    steps.push(Action::Settle(QUIET));
    leave_desktop(&mut steps);
    Interaction { steps }
}

fn model_restart_steps() -> Interaction {
    let mut steps = vec![
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(typed("kill elephant-handler after 20")),
        Action::Keys(typed("desktop")),
        Action::WaitContains("ElephantHandler: warmed up".into()),
        Action::WaitContains("name=elephant-handler by=0".into()),
        Action::WaitContains("Bella: the model service ended; starting it again in 500 ms".into()),
        Action::WaitContains("Bella: starting the model service again".into()),
        Action::WaitContains("ElephantHandler: warmed up".into()),
        Action::Settle(QUIET),
        // Files -> Crash Test -> Bella.
        Action::Keys(vec!["tab", "tab"]),
        Action::Settle(QUIET),
        Action::Keys(typed("Find my pancake recipe")),
        // `pancake recipe` (0.5B) or `pancake` (Qwen3).
        Action::WaitContains("ElephantHandler: chose search `pancake".into()),
        Action::WaitContains("Bella: answered in ".into()),
        Action::Settle(QUIET),
    ];
    leave_desktop(&mut steps);
    Interaction { steps }
}

fn bella_convo_steps() -> Interaction {
    let mut steps = open_bella();
    steps.extend([
        Action::WaitContains("ElephantHandler: warmed up: reply prompt".into()),
        Action::Keys(typed("where is my music")),
        Action::WaitContains("ElephantHandler: chose ".into()),
        Action::WaitContains("ElephantHandler: noted an exchange".into()),
        Action::Settle(QUIET),
        Action::Keys(typed("is that it?")),
        Action::WaitContains("Bella: a question about the last answer".into()),
        Action::WaitContains("ElephantHandler: said `".into()),
        Action::Settle(QUIET),
        Action::NeverSeen("invalid path".into()),
    ]);
    leave_desktop(&mut steps);
    Interaction { steps }
}

fn bella_talk_steps() -> Interaction {
    let mut steps = open_bella();
    steps.extend([
        Action::WaitContains("ElephantHandler: warmed up: reply prompt".into()),
        // Small talk: answered in words, no command asked for.
        Action::Keys(typed("whats your name")),
        Action::WaitContains("ElephantHandler: said `".into()),
        Action::WaitContains("Bella: answered in ".into()),
        Action::Settle(QUIET),
        // No command fits: the model says `none`, then answers in words.
        Action::Keys(typed("Tell me a joke")),
        Action::WaitContains("Bella: no command fits".into()),
        Action::WaitContains("ElephantHandler: said `".into()),
        Action::Settle(QUIET),
        // A sentence that starts with a command word goes to the model.
        Action::Keys(typed("search for my music")),
        Action::WaitContains("ElephantHandler: chose ".into()),
        Action::Settle(QUIET),
        Action::Screenshot("1-talk"),
        Action::NeverSeen("ms by a typed command".into()),
    ]);
    leave_desktop(&mut steps);
    Interaction { steps }
}

fn check_talk(img: &screen::Image) -> Result<String, String> {
    let you = content(img, "You: whats your name")?;
    let joke = content(img, "You: Tell me a joke")?;
    Ok(format!(
        "the conversation is shown: `whats your name` at {you:?}, `Tell me a joke` at {joke:?}"
    ))
}

fn bella_live_steps() -> Interaction {
    let mut steps = open_bella();
    steps.extend([
        // The model warms up in the background as Bella opens
        // (docs/stage2-design.md) ...
        Action::WaitContains("ElephantHandler: warmed up".into()),
        Action::Keys(typed("Find my pancake recipe")),
        // `pancake recipe` (0.5B) or `pancake` (Qwen3).
        Action::WaitContains("ElephantHandler: chose search `pancake".into()),
        // Stage 1 timing lines (docs/stage1-design.md) ...
        Action::WaitContains("ElephantHandler: timing: setup ".into()),
        // ... so the first request takes about as long as later ones: it
        // took 6 s with the set-up in it.
        Action::WaitNumbers("Bella: answered in ".into(), vec![("answered in", 0, 3000)]),
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

/// Bella's idle status line.
const HINT_TEXT: &str =
    "Ask in words, or type a command (search, read, open, move, trash, info...).";

fn check_busy_desktop(img: &screen::Image) -> Result<String, String> {
    let title = title(img, "Files", true)?;
    let sel = selected(img, "Photos", true)?;
    no_selected(img, "Documents", true)?;
    // "Thinking... (Backspace cancels)" until the first batch of the
    // request is read, then "Thinking: reading …" (docs/batch-design.md).
    let busy = content(img, "Thinking")?;
    Ok(format!(
        "while the model works (status `Thinking` at {busy:?}), Files took focus at {title:?} and its selection moved to the 3rd entry at {sel:?}"
    ))
}

fn check_cancelled(img: &screen::Image) -> Result<String, String> {
    let done = content(img, "Cancelled.")?;
    let hint = content(img, HINT_TEXT)?;
    no_content(img, "Thinking")?;
    Ok(format!(
        "`Cancelled.` at {done:?}, ready again at {hint:?}, no longer thinking"
    ))
}

fn check_live(img: &screen::Image) -> Result<String, String> {
    // `pancake recipe` (0.5B) or `pancake` (Qwen3); the result is checked.
    let s = content(img, "Bella: search pancake")?;
    content(img, "  Documents/recipes/pancakes.txt")?;
    let c = content(img, "Bella: create Projects/2026")?;
    let made = content(img, "  created Projects/2026")?;
    no_content(img, "Bella: list")?;
    Ok(format!(
        "model chose `search pancake…` at {s:?} (result shown) and `create Projects/2026` at {c:?}, created after `y` at {made:?}"
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
            // B5 (docs/s0-design.md): 30 s without failing resets the count.
            Action::WaitContains(
                "Zookeeper: badger-handler ran 30 s without failing; restart count reset".into(),
            ),
            Action::Settle(QUIET),
            say("services"),
            wait("badger-handler pid=30 running restarts=0 last=killed"),
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
            Action::WaitFor("ZOO_OS PROC start pid=30 name=bella".into()),
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
            Action::WaitFor("ZOO_OS PROC start pid=35 name=bella".into()),
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
        "ZOO_OS PROC start pid=30 name=bella",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

/// The drag in the mouse case: the Bella's title bar, moved by this.
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
            Action::WaitFor("ZOO_OS PROC start pid=30 name=bella".into()),
            Action::Settle(QUIET),
            Action::Screenshot("1-pointer"),
            // Into the Crash Test window and click.
            p.to(inside(CRASH_WIN)),
            Action::Button(true),
            Action::Button(false),
            Action::WaitContains("PeacockHandler: click focuses Crash Test".into()),
            Action::Settle(QUIET),
            Action::Screenshot("2-click"),
            // Onto the Bella's title bar, then drag it.
            p.to(title_bar(ASSISTANT_WIN)),
            Action::Button(true),
            p.by(DRAG.0, DRAG.1),
            Action::Button(false),
            Action::WaitContains(moved_to("Bella", ASSISTANT_WIN, DRAG)),
            Action::Settle(QUIET),
            Action::Screenshot("3-drag"),
            Action::Keys(vec!["esc"]),
            Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
        ],
    }
}

// ---- D2 partial-redraw cases (docs/d2-design.md §3-4) ------------------

/// The `voice-` cases (docs/listening-design.md): the desktop starts, Bella
/// starts BatHandler, and the microphone plays one sentence. `for_bella`:
/// "Hey Bella, open notes and maximize it." must be carried out; else
/// "Put notes on the left and files on the right." must be heard and left.
fn voice_pause_steps() -> Interaction {
    let mut steps = start_desktop();
    steps.push(Action::WaitContains("BatHandler: listening".into()));
    steps.push(Action::WaitContains("BatHandler: heard her name".into()));
    steps.push(Action::WaitContains(
        "Bella: heard her name; listening for the request".into(),
    ));
    steps.push(Action::WaitContains(
        "Open notes and maximize it.\" after her name".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: 2 steps: Open notes | maximize it".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Notes fills the screen.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

fn voice_confirm_steps() -> Interaction {
    let mut steps = start_desktop();
    steps.push(Action::WaitContains("BatHandler: listening".into()));
    steps.push(Action::WaitContains(
        "Bella: asks: Remove \"milk\" from your notes?".into(),
    ));
    steps.push(Action::WaitContains("BatHandler: heard \"Yes".into()));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Took \"milk\" off \"Shopping: flour, eggs, milk.\" in Documents/notes.txt."
            .into(),
    ));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

fn voice_steps(for_bella: bool) -> Interaction {
    let mut steps = start_desktop();
    // BatHandler says it listens before Bella logs it.
    steps.push(Action::WaitContains("BatHandler: listening".into()));
    steps.push(Action::WaitContains(
        "Bella: listening for \"hey Bella\"".into(),
    ));
    if for_bella {
        // Moonshine writes "Hey Bella," or "Hey, Bella,".
        steps.push(Action::WaitContains(
            "Bella, open notes and maximize it.\" (".into(),
        ));
        steps.push(Action::WaitContains(
            "Bella, open notes and maximize it\"".into(),
        ));
        steps.push(Action::WaitContains(
            "Bella: 2 steps: open notes | maximize it".into(),
        ));
        steps.push(Action::WaitContains("Bella: step: maximize Notes".into()));
        steps.push(Action::WaitContains(
            "Bella: the window server says: Notes fills the screen.".into(),
        ));
        steps.push(Action::Settle(QUIET));
    } else {
        steps.push(Action::WaitContains(
            // The sentence's last mark varies (".", ",").
            "BatHandler: speech not for Bella (".into(),
        ));
        // Its words are not logged (docs/listening-design.md §7); the
        // proof is that she never heard it and nothing moved.
        steps.push(Action::Settle(QUIET));
        steps.push(Action::NeverSeen("Bella: heard".into()));
        steps.push(Action::NeverSeen("PeacockHandler: placed Notes".into()));
    }
    Interaction { steps }
}

fn start_desktop() -> Vec<Action> {
    vec![
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(vec!["d", "e", "s", "k", "t", "o", "p", "ret"]),
        Action::WaitFor("ZOO_OS PROC start pid=30 name=bella".into()),
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
    let sel = selected(img, "Photos", true)?;
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
    let sel = selected(img, "Photos", true)?;
    no_selected(img, "Documents", true)?;
    no_content(img, "Home/Photos")?;
    Ok(format!(
        "back in `Home` from `Photos` with `Photos` selected at {sel:?}"
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
        "ZOO_OS PROC start pid=30 name=bella",
        "ZOO_OS PROC start pid=31 name=notes",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
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

/// 0xRRGGBB as a screenshot pixel.
fn rgb(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

fn check_notes_cursor(img: &screen::Image) -> Result<String, String> {
    // The cursor is a bar between `aX` and `bc` (docs/u3f-design.md).
    let at = content(img, "aX")?;
    let rest = content(img, "bc")?;
    no_content(img, "aXY")?;
    no_content(img, "abcXY")?;
    content(img, "Documents/notes.txt (saved)")?;
    let bar_x = at.0 + screen::ui_width(FaceId::Regular13, "aX") + 1;
    let found = (bar_x..bar_x + 3).any(|x| img.pixel(x, at.1 - 6) == Some(rgb(LIGHT.accent)));
    if rest.1 != at.1 || rest.0 <= at.0 || !found {
        return Err(format!(
            "no cursor bar between `aX` at {at:?} and `bc` at {rest:?} (looked at x {bar_x}..{})",
            bar_x + 3
        ));
    }
    Ok(format!(
        "`aX`, the cursor bar at x {bar_x}, then `bc` at {rest:?}: X inserted two places back, Y deleted at the cursor, saved"
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
    let sel = ui(img, FaceId::Regular13, "Documents", d.select_text, d.select)?;
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
        "Bella",
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
        "dark: `Files` title {t:?}, `Home` {home:?}, `Documents` selected {sel:?}, menu bar {menu:?}, dark wallpaper; no light text left"
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
    let want = title_pen(zui::zoomed(SCREEN.0, SCREEN.1), "Bella");
    let at = title(img, "Bella", true)?;
    if at != want {
        return Err(format!(
            "`Bella` title at {at:?}, expected {want:?} when zoomed"
        ));
    }
    no_title(img, "Files", false)?;
    no_title(img, "Crash Test", false)?;
    Ok(format!(
        "`Bella` zoomed: title at {at:?}, covering Files and Crash Test"
    ))
}

fn check_restored(img: &screen::Image) -> Result<String, String> {
    let want = title_pen(win(ASSISTANT_WIN), "Bella");
    let at = title(img, "Bella", true)?;
    if at != want {
        return Err(format!(
            "`Bella` title at {at:?}, expected {want:?} after restoring"
        ));
    }
    let files = title(img, "Files", false)?;
    Ok(format!(
        "`Bella` restored to {at:?}; `Files` visible again at {files:?}"
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
        "ZOO_OS PROC start pid=30 name=bella",
        "ZOO_OS PROC start pid=31 name=files",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=31 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

/// The `finder` case: Notes (pid 31) opens and lasts to the end.
fn notes_window_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=bella",
        "ZOO_OS PROC start pid=31 name=notes",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=31 name=notes code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

/// The centre-left of Files' sidebar item `k` (first window).
fn files_side(k: usize) -> (usize, usize) {
    let w = win(FILES_WIN);
    (w.0 + 60, zui::line_top(w, k) + zui::LINE_H / 2)
}

/// Content line `k` of the first Files window, with its sidebar.
fn finder_line(k: usize) -> (usize, usize) {
    let w = win(FILES_WIN);
    (
        w.0 + zui::SIDEBAR_W + 60,
        zui::line_top(w, k) + zui::LINE_H / 2,
    )
}

fn finder_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        Action::Screenshot("1-sidebar"),
        // Documents in the sidebar.
        p.to(files_side(2)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("Files: showing Home/Documents".into()),
        // notes.txt, recipes, zoo.rs: a double-click on zoo.rs.
        p.to(finder_line(3)),
        Action::Button(true),
        Action::Button(false),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("Files: opening Documents/zoo.rs in Notes".into()),
        Action::WaitContains("PeacockHandler: Notes opens Documents/zoo.rs".into()),
        Action::WaitContains("Notes: opened Documents/zoo.rs (".into()),
        // Away from the text, then a change saved with Ctrl+S.
        p.to((SCREEN.0 / 2, SCREEN.1 / 2)),
    ]);
    steps.push(Action::Keys(typed("end")));
    steps.extend([
        Action::Key("ctrl", true),
        Action::Keys(vec!["s"]),
        Action::Key("ctrl", false),
        // 360 bytes, `end` and the Enter `typed` adds.
        Action::WaitContains("Notes: saved 364 bytes".into()),
        Action::Settle(QUIET),
        Action::Screenshot("2-code"),
    ]);
    // A right-click on Files' empty space (left of the Notes window).
    let w = win(FILES_WIN);
    let at = (w.0 + zui::SIDEBAR_W + 100, zui::line_top(w, 8));
    let seps = [false, false];
    steps.extend([
        p.to(at),
        Action::RightButton(true),
        Action::RightButton(false),
        Action::WaitContains("PeacockHandler: menu Files: New Folder, New Text File".into()),
        p.to((
            at.0 + 90,
            zui::menu_item_top(at.1 + 2, &seps, 0) + zui::MENU_ITEM_H / 2,
        )),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("Files: created folder Documents/untitled folder".into()),
        p.to((SCREEN.0 / 2, SCREEN.1 - 200)),
        Action::Settle(QUIET),
        Action::Screenshot("3-folder"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

/// Files at start: the sidebar (Home selected), a folder icon, the kind
/// column.
fn check_finder(img: &screen::Image) -> Result<String, String> {
    let fav = ui(
        img,
        FaceId::Regular13,
        "Favourites",
        LIGHT.text_dim,
        LIGHT.sidebar,
    )?;
    let docs = ui(
        img,
        FaceId::Regular13,
        "Documents",
        LIGHT.text,
        LIGHT.sidebar,
    )?;
    let home = ui(
        img,
        FaceId::Regular13,
        "Home",
        LIGHT.text,
        LIGHT.sidebar_select,
    )?;
    let kind = selected(img, "Folder", true)?;
    // Documents' folder icon: its light front.
    let w = win(FILES_WIN);
    let (x0, y0) = (
        w.0 + 1 + zui::SIDEBAR_W + zui::TEXT_X,
        zui::line_top(w, 1) + 3,
    );
    let front = rgb(0x7DC2F7);
    let icon = (y0..y0 + 16).any(|y| (x0..x0 + 16).any(|x| img.pixel(x, y) == Some(front)));
    if !icon {
        return Err(format!("no folder icon at ({x0}, {y0})"));
    }
    Ok(format!(
        "sidebar: `Favourites` {fav:?}, `Home` selected {home:?}, `Documents` {docs:?}; folder icon at ({x0}, {y0}); `Folder` {kind:?}"
    ))
}

/// zoo.rs in Notes: keywords, types, strings, comments, macros and
/// functions each in their colour.
fn check_code_colours(img: &screen::Image) -> Result<String, String> {
    let t = title(img, "zoo.rs", true)?;
    content(img, "Documents/zoo.rs (saved)")?;
    let k = &LIGHT.syntax;
    let mut found = Vec::new();
    for (text, c, role) in [
        ("struct", k[0], "keyword"),
        ("\"Peacock\"", k[1], "string"),
        // Whole: the long line wraps between words (docs/u4b-design.md).
        ("\"Badger\"", k[1], "string after a word wrap"),
        (
            "// A small program, to show Notes' colours.",
            k[2],
            "comment",
        ),
        ("u32", k[4], "type"),
        ("println!", k[6], "macro"),
        ("main", k[5], "function"),
    ] {
        let at = ui(img, FaceId::Regular13, text, c, LIGHT.content)
            .map_err(|e| format!("{role}: {e}"))?;
        found.push(format!("{role} `{text}` {at:?}"));
    }
    // Not all one colour: the keyword is not in the text colour.
    no_ui(img, FaceId::Regular13, "struct", LIGHT.text, LIGHT.content)?;
    content(img, "end")?;
    Ok(format!("`zoo.rs` {t:?}; {}", found.join(", ")))
}

fn check_new_folder(img: &screen::Image) -> Result<String, String> {
    let at = selected(img, "untitled folder", true)?;
    content(img, "Home/Documents")?;
    Ok(format!("`untitled folder` made and selected at {at:?}"))
}

/// The `bella-memory` case: the desktop runs twice; each run starts
/// the broker and the model service (which reports no model).
fn bella_memory_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    for base in [26, 33] {
        let pid = |k: usize| base + k;
        let lines = [
            "ZOO_OS SHELL line=desktop".to_string(),
            format!("ZOO_OS PROC start pid={} name=zookeeper", pid(0)),
            format!("ZOO_OS PROC start pid={} name=peacock-handler", pid(1)),
            format!(
                "ZOO_OS DISPLAY acquired pid={} name=peacock-handler",
                pid(1)
            ),
            format!("ZOO_OS PROC start pid={} name=files", pid(2)),
            format!("ZOO_OS PROC start pid={} name=crash-test", pid(3)),
            format!("ZOO_OS PROC start pid={} name=bella", pid(4)),
            format!("ZOO_OS PROC start pid={} name=otter-handler", pid(5)),
            format!("ZOO_OS PROC start pid={} name=elephant-handler", pid(6)),
            format!(
                "ZOO_OS PROC exit pid={} name=peacock-handler code=0",
                pid(1)
            ),
            format!("ZOO_OS DISPLAY released pid={}", pid(1)),
        ];
        e.then(&lines.iter().map(String::as_str).collect::<Vec<_>>());
        let exits = [
            format!("ZOO_OS PROC exit pid={} name=files code=0", pid(2)),
            format!("ZOO_OS PROC exit pid={} name=crash-test code=0", pid(3)),
            format!("ZOO_OS PROC exit pid={} name=bella code=0", pid(4)),
            format!("ZOO_OS PROC exit pid={} name=otter-handler code=0", pid(5)),
            format!(
                "ZOO_OS PROC exit pid={} name=elephant-handler code=0",
                pid(6)
            ),
            format!("ZOO_OS PROC exit pid={} name=zookeeper code=0", pid(0)),
        ];
        e.then_any_order(&exits.iter().map(String::as_str).collect::<Vec<_>>());
    }
    e
}

fn bella_memory_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    // Not answered (no model in this image) ...
    steps.push(Action::Keys(typed("where is my pancake recipe")));
    steps.push(Action::WaitFor(
        "ZOO_OS PROC start pid=32 name=elephant-handler".into(),
    ));
    steps.push(Action::Settle(QUIET));
    // ... so the command typed next is its answer.
    steps.push(Action::Keys(typed("search pancake")));
    steps.push(Action::WaitContains(
        "Bella: learned `where is my pancake recipe` -> `search pancake`".into(),
    ));
    // The same request in other words: from memory.
    steps.push(Action::Keys(typed("where is the pancake recipe")));
    steps.push(Action::WaitContains(
        "Bella: recalled `search pancake` (100% alike)".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("1-recalled"));
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()));
    steps.push(Action::WaitFor(
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0".into(),
    ));
    // A new desktop: the memory is on disk.
    steps.push(Action::Keys(typed("desktop")));
    steps.push(Action::WaitFor(
        "ZOO_OS PROC start pid=37 name=bella".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("pancake recipe where is it")));
    steps.push(Action::WaitContains(
        "Bella: recalled `search pancake` (80% alike)".into(),
    ));
    // Unrelated: not from memory, so the model is asked.
    steps.push(Action::Keys(typed(
        "make a new project folder for work called Invoices",
    )));
    steps.push(Action::WaitFor(
        "ZOO_OS PROC start pid=39 name=elephant-handler".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("2-after-restart"));
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitFor("ZOO_OS DISPLAY released pid=34".into()));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

fn check_recalled(img: &screen::Image) -> Result<String, String> {
    let at = content(img, "Bella (remembered): search pancake")?;
    let found = content(img, "  Documents/recipes/pancakes.txt")?;
    Ok(format!(
        "answered from memory at {at:?}, with the search result at {found:?}"
    ))
}

fn check_recalled_again(img: &screen::Image) -> Result<String, String> {
    let at = content(img, "Bella (remembered): search pancake")?;
    content(img, "  Documents/recipes/pancakes.txt")?;
    let model = content(img, "The model is not installed; type a command instead.")?;
    no_content(img, "Bella (remembered): make")?;
    Ok(format!(
        "after a restart, answered from memory at {at:?}; the unrelated request went to the model ({model:?})"
    ))
}

fn any_case_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=bella",
        "ZOO_OS PROC start pid=31 name=otter-handler",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC exit pid=31 name=otter-handler code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

fn notes_large_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        // Music in the sidebar; liner-notes.txt is its first entry.
        p.to(files_side(3)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("Files: showing Home/Music".into()),
        p.to(finder_line(1)),
        Action::Button(true),
        Action::Button(false),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("Notes: opened Music/liner-notes.txt (9230 bytes)".into()),
        p.to((SCREEN.0 / 2, SCREEN.1 / 2)),
    ]);
    steps.push(Action::Keys(typed("end")));
    steps.extend([
        Action::Key("ctrl", true),
        Action::Keys(vec!["s"]),
        Action::Key("ctrl", false),
        // 9,230 bytes, `end` and the Enter `typed` adds.
        Action::WaitContains("Notes: saved 9234 bytes".into()),
        Action::Settle(QUIET),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

/// The desktop starts; after that, process numbers depend on what is
/// opened.
fn desktop_then_rest(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
    ]);
    e.then_rest();
    e
}

fn bella_audit_steps() -> Interaction {
    let mut steps = start_desktop();
    let say = |steps: &mut Vec<Action>, line: &str, wait: &str| {
        steps.push(Action::Keys(typed(line)));
        steps.push(Action::WaitContains(wait.into()));
        steps.push(Action::Settle(QUIET));
    };
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    say(
        &mut steps,
        "what did you do?",
        "OtterHandler: listed 0 of 0 changes",
    );
    // A read: a log line, no audit line.
    say(
        &mut steps,
        "list Documents",
        "OtterHandler: audit: list Documents: done",
    );
    // A change: both.
    say(
        &mut steps,
        "remember that Zara the zebra likes carrots",
        "OtterHandler: audit: fact-add Zara the zebra likes carrots: done",
    );
    // A new desktop: the audit file is on disk.
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitContains(
        "Zookeeper: desktop session ended".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("desktop")));
    steps.push(Action::WaitContains("name=bella".into()));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    say(
        &mut steps,
        "what have you changed?",
        "OtterHandler: listed 1 of 1 changes",
    );
    // The one line kept is the dated fact-add; the list was not a change.
    steps.push(Action::WaitSeen("OtterHandler: shows change: 20".into()));
    Interaction { steps }
}

/// `bella-windows`: the typed window actions.
fn bella_windows_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("maximize notes")));
    steps.push(Action::WaitContains(
        "Bella: asked to maximize notes".into(),
    ));
    steps.push(Action::WaitContains("PeacockHandler: opened Notes".into()));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Notes at 24x".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Notes fills the screen.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("maximized"));
    // Notes has the keyboard and covers Bella: Tab past Files and Crash
    // Test (Network is not open) back to Bella.
    steps.push(Action::Keys(vec!["tab", "tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("minimize notes")));
    steps.push(Action::WaitContains(
        "PeacockHandler: minimized Notes".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Minimized Notes; it is in the dock.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("close notes")));
    steps.push(Action::WaitContains("PeacockHandler: closed Notes".into()));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Closed Notes.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("close notes")));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Notes is not open.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

/// Measures the tone QEMU recorded (docs/audio-design.md): the loud part of
/// the first channel must last at least 0.8 s, and its pitch, from
/// rising zero crossings, be within 2% of `hz`.
fn check_tone(wav: &Path, hz: f64) -> Result<String, String> {
    let b = std::fs::read(wav).map_err(|e| format!("no recording: {e}"))?;
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let (mut rate, mut channels, mut bits) = (0u32, 0u16, 0u16);
    let mut data: &[u8] = &[];
    let mut at = 12;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = at + 8;
        if id == b"fmt " && body + 16 <= b.len() {
            channels = u16::from_le_bytes([b[body + 2], b[body + 3]]);
            rate = u32::from_le_bytes(b[body + 4..body + 8].try_into().unwrap());
            bits = u16::from_le_bytes([b[body + 14], b[body + 15]]);
        } else if id == b"data" {
            // QEMU fills in the length when it closes the file; a killed
            // QEMU leaves it 0: then the rest of the file.
            let end = if len == 0 || body + len > b.len() {
                b.len()
            } else {
                body + len
            };
            data = &b[body..end];
            break;
        }
        at = body + len + (len & 1);
    }
    if rate == 0 || channels == 0 || bits != 16 {
        return Err(format!(
            "unexpected format: {rate} Hz, {channels} channels, {bits} bits"
        ));
    }
    let frame = 2 * usize::from(channels);
    let left: Vec<f64> = data
        .chunks_exact(frame)
        .map(|f| f64::from(i16::from_le_bytes([f[0], f[1]])))
        .collect();
    // Loud samples: above a tenth of the tone's level (12 000).
    let loud: Vec<usize> = (0..left.len())
        .filter(|&i| left[i].abs() > 1_200.0)
        .collect();
    let (Some(&first), Some(&last)) = (loud.first(), loud.last()) else {
        return Err(format!(
            "no tone: {:.2} s recorded, all quiet",
            left.len() as f64 / f64::from(rate)
        ));
    };
    let secs = (last - first) as f64 / f64::from(rate);
    let rising = (first + 1..=last)
        .filter(|&i| left[i - 1] < 0.0 && left[i] >= 0.0)
        .count();
    let measured = rising as f64 / secs.max(1e-9);
    if secs < 0.8 {
        return Err(format!("tone too short: {secs:.2} s"));
    }
    if (measured - hz).abs() > hz * 0.02 {
        return Err(format!("tone at {measured:.1} Hz, wanted {hz:.0} Hz"));
    }
    Ok(format!(
        "{measured:.1} Hz for {secs:.2} s ({rate} Hz, {channels} channels recorded)"
    ))
}

/// `bella-events`: "it" from what the user did.
fn bella_events_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files has the focus: Applications (last in Home), then Notes (the
    // fourth application), opened with Enter.
    steps.push(Action::Keys(vec!["down"; 16]));
    steps.push(Action::Keys(vec!["ret"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["down"; 3]));
    steps.push(Action::Keys(vec!["ret"]));
    steps.push(Action::WaitContains("PeacockHandler: opened Notes".into()));
    steps.push(Action::Settle(QUIET));
    // Notes has the keyboard: a click on Bella's title bar, as a user
    // would (Tab would pass through Files and focus it on the way).
    let mut p = Ptr::start();
    steps.push(p.to(title_bar(ASSISTANT_WIN)));
    steps.push(Action::Button(true));
    steps.push(Action::Button(false));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("maximize it")));
    steps.push(Action::WaitContains(
        "Bella: \"it\" is Notes (from the desktop's events)".into(),
    ));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Notes at 24x38 1872x948".into(),
    ));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

/// `bella-actions`: a line written in Notes for Bella.
/// `bella-layout` (docs/layouts-design.md): typed, no model. Three
/// windows (Files, Crash Test, Bella) in the 1872x948 space from 24x38.
fn bella_layout_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    // Rows: three stacked, 308 px each with 12 px between.
    steps.push(Action::Keys(typed("put these in rows")));
    steps.push(Action::WaitContains(
        "Bella: read the layout v(...) from the words".into(),
    ));
    steps.push(Action::WaitContains("at 24x358 1872x308".into()));
    steps.push(Action::WaitContains(
        "Bella: asks: Is this what you meant?".into(),
    ));
    steps.push(Action::Keys(vec!["y"]));
    steps.push(Action::WaitContains(
        "Bella: learned `put these in rows` -> `layout v(...)`".into(),
    ));
    steps.push(Action::Settle(QUIET));
    // Two on top, one below: arranged, asked, and not what was meant.
    steps.push(Action::Keys(typed(
        "split it three ways, two on top and one on the bottom",
    )));
    steps.push(Action::WaitContains(
        // "it" is the app the desktop's events name: Files.
        "Bella: read the layout v(h(files,*),*) from the words".into(),
    ));
    steps.push(Action::WaitContains("at 966x38 930x468".into()));
    steps.push(Action::WaitContains("at 24x518 1872x468".into()));
    steps.push(Action::WaitContains(
        "Bella: asks: Is this what you meant?".into(),
    ));
    steps.push(Action::Keys(vec!["n"]));
    steps.push(Action::WaitContains(
        "Bella: answered no: How should they go?".into(),
    ));
    steps.push(Action::Settle(QUIET));
    // The correction: confirmed, it answers both.
    steps.push(Action::Keys(typed("stack them")));
    steps.push(Action::WaitContains(
        "Bella: asks: Is this what you meant?".into(),
    ));
    steps.push(Action::Keys(vec!["y"]));
    steps.push(Action::WaitContains(
        "Bella: learned `stack them` -> `layout v(...)`".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: learned `split Files three ways, two on top and one on the bottom` -> `layout v(...)`"
            .into(),
    ));
    steps.push(Action::Settle(QUIET));
    // Learned: from memory, without a question.
    steps.push(Action::Keys(typed("put these in rows please")));
    steps.push(Action::WaitContains(
        "Bella: recalled `layout v(...)`".into(),
    ));
    steps.push(Action::Settle(QUIET));
    // The layout answered "no" was never learned.
    steps.push(Action::NeverSeen("-> `layout v(h(files".into()));
    steps.push(Action::Screenshot("rows"));
    Interaction { steps }
}

/// `bella-one` (docs/one-bella-design.md): typed, so no model.
fn bella_one_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    // Ctrl+N on Bella (she has the keyboard): her one window, not a new one.
    steps.push(Action::Keys(vec!["ctrl+n"]));
    steps.push(Action::WaitContains(
        "PeacockHandler: Bella is one; showing her window".into(),
    ));
    steps.push(Action::Settle(QUIET));
    // A split naming her twice: one Bella column, then Files.
    steps.push(Action::Keys(typed("split bella and bella and files")));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Bella at 24x38".into(),
    ));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Files at".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::NeverSeen("new window Bella".into()));
    steps.push(Action::NeverSeen("another Bella is running".into()));
    steps.push(Action::Screenshot("one-bella"));
    Interaction { steps }
}

/// `bella-notes` (docs/registry-design.md): typed commands, so no model;
/// Notes comes to the front for each action, so Bella is clicked again
/// before the next.
fn bella_notes_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    // One pointer for every click: it moves relative to where it is.
    let mut p = Ptr::start();
    let mut back_to_bella = |steps: &mut Vec<Action>| {
        steps.push(p.to(title_bar(ASSISTANT_WIN)));
        steps.push(Action::Button(true));
        steps.push(Action::Button(false));
        steps.push(Action::Settle(QUIET));
    };
    steps.push(Action::Keys(typed("write flowers, egg and milk")));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Wrote \"flowers, egg and milk\" in Documents/notes.txt."
            .into(),
    ));
    steps.push(Action::Settle(QUIET));
    back_to_bella(&mut steps);
    steps.push(Action::Keys(typed("change egg to bread")));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Changed \"egg\" to \"bread\" in Documents/notes.txt."
            .into(),
    ));
    steps.push(Action::Settle(QUIET));
    back_to_bella(&mut steps);
    // A delete: asked first; y goes ahead.
    steps.push(Action::Keys(typed("remove milk")));
    steps.push(Action::WaitContains(
        "Bella: asks: Remove \"milk\" from your notes?".into(),
    ));
    steps.push(Action::Keys(vec!["y"]));
    // Two lines hold "milk" (the seeded shopping line and the one written
    // above): Notes changes nothing and Bella asks which
    // (docs/targets-design.md).
    steps.push(Action::WaitContains(
        "Bella: asks: Which one: 1. Shopping: flour, eggs, milk. | 2. flowers, bread and milk"
            .into(),
    ));
    steps.push(Action::Settle(QUIET));
    back_to_bella(&mut steps);
    steps.push(Action::Keys(vec!["2"]));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Took \"milk\" off \"flowers, bread and milk\" in Documents/notes.txt."
            .into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::NeverSeen("Took \"milk\" off \"Shopping".into()));
    back_to_bella(&mut steps);
    // A request not to: nothing done.
    steps.push(Action::Keys(typed("don't remove flowers")));
    steps.push(Action::WaitContains(
        "Bella: a request not to: nothing done".into(),
    ));
    steps.push(Action::Settle(QUIET));
    // n keeps it.
    steps.push(Action::Keys(typed("remove flowers")));
    steps.push(Action::WaitContains(
        "Bella: asks: Remove \"flowers\" from your notes?".into(),
    ));
    steps.push(Action::Keys(vec!["n"]));
    steps.push(Action::WaitContains("Bella: answered no: Kept.".into()));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::NeverSeen("Took \"flowers\"".into()));
    // In Notes' registered words, with no model in this image: "add" is a
    // verb of `write` and "to my notes" one of its places
    // (docs/regression-design.md).
    back_to_bella(&mut steps);
    steps.push(Action::Keys(typed("add bread to my notes")));
    steps.push(Action::WaitContains(
        "Bella: in the registry's words: write bread".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Wrote \"bread\" in Documents/notes.txt.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("edited"));
    Interaction { steps }
}

fn bella_actions_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("write down buy milk")));
    steps.push(Action::WaitContains(
        "PeacockHandler: Bella asks Notes: do:write:buy milk".into(),
    ));
    steps.push(Action::WaitContains(
        "Notes: wrote a line in Documents/notes.txt".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Wrote \"buy milk\" in Documents/notes.txt.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("written"));
    Interaction { steps }
}

/// `bella-actions-model`: a note in words.
fn bella_actions_model_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::WaitContains(
        "ElephantHandler: warmed up: system prompt".into(),
    ));
    steps.push(Action::Keys(typed(
        "hey bella take a note that the dentist is on friday",
    )));
    steps.push(Action::WaitContains(
        "ElephantHandler: chose write `the dentist is on friday`".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Wrote \"the dentist is on friday\" in Documents/notes.txt."
            .into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("noted"));
    Interaction { steps }
}

/// `net-bella-browse`: a page fetched in Network for Bella.
fn bella_browse_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("browse 10.0.2.100")));
    steps.extend([
        Action::WaitFor("ZOO_OS PROC start pid=31 name=network".into()),
        Action::WaitContains("Network: connected 10.0.2.15 via 10.0.2.2".into()),
        Action::WaitContains(
            "Network: fetched http://10.0.2.100/ (10.0.2.100): 92 bytes, `HTTP/1.0 200 OK`".into(),
        ),
        Action::WaitContains("Bella: the window server says: Fetched 10.0.2.100:".into()),
        Action::Settle(QUIET),
        Action::Screenshot("1-fetched"),
        // Network has the keyboard: Esc ends the session.
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

/// `gpu-monitors-model`: the owner's example, three monitors.
fn gpu_monitors_model_steps() -> Interaction {
    let mut steps = start_desktop();
    steps.push(Action::WaitSeen(
        "ZOO_OS INFO gpu scanout 5760x1080 hardware-cursor monitors=3".into(),
    ));
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::WaitContains(
        "ElephantHandler: warmed up: system prompt".into(),
    ));
    steps.push(Action::Keys(typed(
        "hey bella put notes maximized on monitor 3 please",
    )));
    // Monitor 3's zoomed space: x 3840 + 24.
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Notes at 3864x38 1872x948".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Notes fills the screen of monitor 3.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("notes-on-3"));
    // Notes has the keyboard: Tab past Files and Crash Test to Bella.
    steps.push(Action::Keys(vec!["tab", "tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed(
        "can you move files to the second screen",
    )));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Files is on monitor 2.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("files-on-2"));
    Interaction { steps }
}

/// `gpu-monitors`: two monitors.
fn gpu_monitors_steps() -> Interaction {
    let mut steps = start_desktop();
    // Both said before Bella starts (`start_desktop` waits for that).
    steps.push(Action::WaitSeen(
        "ZOO_OS INFO gpu scanout 3840x1080 hardware-cursor monitors=2".into(),
    ));
    steps.push(Action::WaitSeen(
        "PeacockHandler: display 3840x1080 acquired".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("desktop"));
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    // Monitor 2's zoomed space: x 1920 + 24, 1872 wide.
    steps.push(Action::Keys(typed("maximize notes on monitor 2")));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Notes at 1944x38 1872x948".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Notes fills the screen of monitor 2.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("notes-on-2"));
    // Notes has the keyboard: Tab past Files and Crash Test to Bella.
    steps.push(Action::Keys(vec!["tab", "tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("split notes and files on monitor 2")));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Files at 2886x38 930x948".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("split-on-2"));
    // Files has the keyboard: Tab past Crash Test to Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("show files on monitor 3")));
    steps.push(Action::WaitContains(
        "Bella: the window server says: There is no monitor 3; this screen has 2.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

/// `bella-steps`: requests of several steps, typed commands only.
fn bella_steps_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed(
        "launch notes and files, then minimize files",
    )));
    steps.push(Action::WaitContains(
        "Bella: 2 steps: launch notes and files | minimize files".into(),
    ));
    steps.push(Action::WaitContains("Bella: step: minimize files".into()));
    steps.push(Action::WaitContains("PeacockHandler: opened Notes".into()));
    steps.push(Action::WaitContains(
        "PeacockHandler: minimized Files".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("launched-minimized"));
    // The apps took the keyboard: back to Bella.
    let mut p = Ptr::start();
    steps.push(p.to(title_bar(ASSISTANT_WIN)));
    steps.push(Action::Button(true));
    steps.push(Action::Button(false));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("open notes and maximize it")));
    steps.push(Action::WaitContains(
        "Bella: 2 steps: open notes | maximize it".into(),
    ));
    // "it": the app the step before named.
    steps.push(Action::WaitContains("Bella: step: maximize Notes".into()));
    steps.push(Action::WaitContains(
        "Notes: opened Documents/notes.txt".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Notes fills the screen.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

/// `bella-steps-model`: several steps in words.
fn bella_steps_model_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::WaitContains(
        "ElephantHandler: warmed up: system prompt".into(),
    ));
    steps.push(Action::Keys(typed(
        "hey bella open notes and files, then put them side by side",
    )));
    steps.push(Action::WaitContains(
        "Bella: 2 steps: open notes and files | put them side by side".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: step: put Notes and Files side by side".into(),
    ));
    steps.push(Action::WaitContains(
        "ElephantHandler: chose split `Notes and Files`".into(),
    ));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Files at 966x38 930x948".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("side-by-side"));
    Interaction { steps }
}

/// `bella-tiling`: the typed splits.
fn bella_tiling_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("split notes and bella")));
    steps.push(Action::WaitContains("PeacockHandler: opened Notes".into()));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Notes at 24x".into(),
    ));
    // 1920x1080: halves of the zoomed space, 12 px apart.
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Bella at 966x38 930x948".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Bella is on the right.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("halves"));
    // Bella, on the right, has the keyboard.
    steps.push(Action::Keys(typed("split files, notes and network")));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Network at 1280x38 616x948".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Notes is in the middle.".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Network is on the right.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("thirds"));
    // Network has the keyboard: Tab past Files and Crash Test to Bella.
    steps.push(Action::Keys(vec!["tab", "tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("split")));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Crash Test is on the left and Files on the right.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("front-two"));
    // Crash Test, on the left, has the keyboard: Tab to Bella. Two
    // windows of one app: the second is opened.
    steps.push(Action::Keys(vec!["tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("split files and files")));
    steps.push(Action::WaitContains(
        "PeacockHandler: new window Files".into(),
    ));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Files at 966x38 930x948".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("two-files"));
    Interaction { steps }
}

/// `bella-tiling-model`: the owner's example, in words.
fn bella_tiling_model_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::WaitContains(
        "ElephantHandler: warmed up: system prompt".into(),
    ));
    steps.push(Action::Keys(typed(
        "put notes on the left and the assistant on the right",
    )));
    steps.push(Action::WaitContains(
        "ElephantHandler: chose split `Notes and Bella`".into(),
    ));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Notes at 24x".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Bella is on the right.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("halves"));
    // No app named: the two front windows other than Bella's, Notes first
    // (an empty argument used to count as "no command fits").
    steps.push(Action::Keys(typed(
        "hey bella can you split these 2 windows",
    )));
    steps.push(Action::WaitContains(
        "ElephantHandler: chose split ``".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Notes is on the left and".into(),
    ));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

/// `bella-windows-model`: the owner's example, in words.
fn bella_windows_model_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::WaitContains(
        "ElephantHandler: warmed up: system prompt".into(),
    ));
    steps.push(Action::Keys(typed(
        "open notes and maximize it on my screen",
    )));
    steps.push(Action::WaitContains(
        "ElephantHandler: chose maximize `Notes`".into(),
    ));
    steps.push(Action::WaitContains(
        "PeacockHandler: placed Notes at 24x".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the window server says: Notes fills the screen.".into(),
    ));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

fn bella_context_steps(model: bool) -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    if model {
        steps.push(Action::WaitContains(
            "ElephantHandler: warmed up: reply prompt".into(),
        ));
    }
    steps.push(Action::Keys(typed("open Documents/zoo.rs")));
    steps.push(Action::WaitContains(
        "Notes: opened Documents/zoo.rs".into(),
    ));
    steps.push(Action::Settle(QUIET));
    // Notes has the keyboard now: back to Bella.
    let mut p = Ptr::start();
    steps.push(p.to(title_bar(ASSISTANT_WIN)));
    steps.push(Action::Button(true));
    steps.push(Action::Button(false));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("what am I working on?")));
    // Said at once, before the snapshot (docs/stream-design.md).
    steps.push(Action::WaitContains(
        "Bella: said first: Let me look at your screen...".into(),
    ));
    steps.push(Action::WaitContains(
        "PeacockHandler: context for Bella".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: the screen: focused: Bella; open: ".into(),
    ));
    steps.push(Action::WaitSeen("opened Documents/zoo.rs in Notes".into()));
    if model {
        // After the screen line, the next one naming zoo is her reply.
        steps.push(Action::WaitContains("zoo".into()));
        steps.push(Action::Settle(QUIET));
        steps.push(Action::WaitSeen("ElephantHandler: said `".into()));
    }
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

fn bella_facts_steps() -> Interaction {
    let mut steps = start_desktop();
    let say = |steps: &mut Vec<Action>, line: &str, wait: &str| {
        steps.push(Action::Keys(typed(line)));
        steps.push(Action::WaitContains(wait.into()));
        steps.push(Action::Settle(QUIET));
    };
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    say(
        &mut steps,
        "remember that my dentist appointment is on Friday",
        "OtterHandler: remembered a fact (1 kept)",
    );
    say(
        &mut steps,
        "remember that Zara the zebra likes carrots",
        "OtterHandler: remembered a fact (2 kept)",
    );
    say(
        &mut steps,
        "what do you remember?",
        "OtterHandler: listed 2 facts",
    );
    // A question the facts answer: the matching fact goes with it (no
    // model in this image, so no reply in words).
    say(
        &mut steps,
        "when is my dentist appointment",
        "Bella: from what you told me: my dentist appointment is on Friday",
    );
    steps.push(Action::NeverSeen("Zara the zebra likes carrots;".into()));
    // A new desktop: the facts are on disk.
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitContains(
        "Zookeeper: desktop session ended".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("desktop")));
    steps.push(Action::WaitContains("name=bella".into()));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    say(
        &mut steps,
        "what do you remember?",
        "OtterHandler: listed 2 facts",
    );
    // Forgetting asks first.
    steps.push(Action::Keys(typed("forget my dentist appointment")));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["n"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::NeverSeen("OtterHandler: forgot".into()));
    steps.push(Action::Keys(typed("forget my dentist appointment")));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["y"]));
    steps.push(Action::WaitContains("OtterHandler: forgot 1 facts".into()));
    steps.push(Action::Settle(QUIET));
    say(
        &mut steps,
        "what do you remember?",
        "OtterHandler: listed 1 facts",
    );
    Interaction { steps }
}

fn bella_lookup_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("what do my notes say about the vet?")));
    steps.push(Action::WaitContains(
        "Bella: said first: Let me check your files...".into(),
    ));
    steps.push(Action::WaitContains(
        "OtterHandler: lookup `vet` found 1 lines in ".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: answering from Documents/notes.txt".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("did I write anything about unicorns")));
    steps.push(Action::WaitContains(
        "OtterHandler: lookup `anything unicorns` found 0 lines in ".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: nothing in the files for that".into(),
    ));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

/// Typed while the models load (`smp-keys`).
const KEYS_UNDER_LOAD: &str = "find my pancake recipe please and thank you very much";

fn bella_vectors_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::WaitContains(
        "MagpieHandler: read the embedding model from the model disk".into(),
    ));
    steps.push(Action::WaitContains("MagpieHandler: index: ".into()));
    steps.push(Action::WaitContains(
        "Bella: the file index is ready (".into(),
    ));
    // The chat model warms up first: the index runs only when idle.
    steps.push(Action::WaitSeen(
        "ElephantHandler: warmed up: reply prompt".into(),
    ));
    // No word of the question is in the vet line: only meaning finds it.
    steps.push(Action::Keys(typed(
        "what do my notes say about the animal doctor?",
    )));
    steps.push(Action::WaitContains("MagpieHandler: query: ".into()));
    steps.push(Action::WaitContains(
        "Bella: answering from Documents/notes.txt".into(),
    ));
    steps.push(Action::WaitContains(
        "ElephantHandler: answered from files `".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::WaitSeenAny(vec!["Zara".into(), "zebra".into()]));
    // A new desktop: the index comes back from `.bella-index`.
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitContains(
        "Zookeeper: desktop session ended".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("desktop")));
    steps.push(Action::WaitContains("name=magpie-handler".into()));
    steps.push(Action::WaitContains(
        "MagpieHandler: read the embedding model".into(),
    ));
    // Nothing embedded again: every line's vector came from the file.
    steps.push(Action::WaitContains(" (0 embedded, ".into()));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

fn bella_lookup_answer_steps() -> Interaction {
    let mut steps = open_bella();
    steps.extend([
        Action::WaitContains("ElephantHandler: warmed up: reply prompt".into()),
        Action::Keys(typed("what do my notes say about the vet?")),
        Action::WaitContains("Bella: answering from Documents/notes.txt".into()),
        Action::WaitContains("ElephantHandler: answered from files `".into()),
        Action::Settle(QUIET),
        // The file's lines are not in the log: "Zara" can only come
        // from the file through the answer.
        Action::WaitSeenAny(vec!["Zara".into(), "zebra".into()]),
    ]);
    leave_desktop(&mut steps);
    Interaction { steps }
}

fn bella_summary_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed(
        "summarise the file Documents/recipes/pancakes.txt",
    )));
    // Said at once, before the slow part (docs/stream-design.md).
    steps.push(Action::WaitContains(
        "Bella: said first: Let me read that file, one moment...".into(),
    ));
    // The file is found by its path; its text goes to the model service.
    steps.push(Action::WaitContains(
        "Bella: summarising Documents/recipes/pancakes.txt (".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("summarize nothing-here.txt")));
    steps.push(Action::WaitContains(
        "OtterHandler: audit: read nothing-here.txt: not found".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::NeverSeen(
        "Bella: summarising nothing-here.txt".into(),
    ));
    Interaction { steps }
}

fn bella_summary_answer_steps() -> Interaction {
    let mut steps = open_bella();
    steps.extend([
        Action::WaitContains("ElephantHandler: warmed up: reply prompt".into()),
        Action::Keys(typed("summarise Documents/notes.txt")),
        Action::WaitContains("Bella: summarising Documents/notes.txt".into()),
        // The summary appears as it is written (docs/stream-design.md).
        Action::WaitContains("Bella: first words on screen ".into()),
        Action::WaitContains("ElephantHandler: summarised `".into()),
        Action::Settle(QUIET),
        // Words only the file holds: its text is not in the log, so they
        // can only come from the summary. Models word it differently
        // (0.5B: "shopping"; Qwen3-1.7B: "flour, eggs, and milk").
        Action::WaitSeenAny(vec!["hopping".into(), "flour".into(), "the vet".into()]),
    ]);
    leave_desktop(&mut steps);
    Interaction { steps }
}

fn bella_save_steps() -> Interaction {
    let mut steps = open_bella();
    let summarise = |steps: &mut Vec<Action>| {
        steps.push(Action::Keys(typed("summarise Documents/notes.txt")));
        steps.push(Action::WaitContains("ElephantHandler: summarised `".into()));
        steps.push(Action::Settle(QUIET));
    };
    steps.push(Action::WaitContains(
        "ElephantHandler: warmed up: reply prompt".into(),
    ));
    summarise(&mut steps);
    steps.push(Action::Keys(typed("save that")));
    steps.push(Action::WaitContains(
        "OtterHandler: wrote Documents/notes-summary.txt (".into(),
    ));
    steps.push(Action::Settle(QUIET));
    // The same summary again: the file exists, so it is not replaced.
    summarise(&mut steps);
    steps.push(Action::Keys(typed("save it")));
    steps.push(Action::WaitContains(
        "OtterHandler: audit: write Documents/notes-summary.txt: already exists".into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("open Documents/notes-summary.txt")));
    steps.push(Action::WaitContains(
        "Notes: opened Documents/notes-summary.txt".into(),
    ));
    steps.push(Action::Settle(QUIET));
    leave_desktop(&mut steps);
    Interaction { steps }
}

fn bella_facts_answer_steps() -> Interaction {
    let mut steps = open_bella();
    steps.extend([
        Action::WaitContains("ElephantHandler: warmed up: reply prompt".into()),
        Action::Keys(typed("remember that my dentist appointment is on Friday")),
        Action::WaitContains("OtterHandler: remembered a fact".into()),
        Action::Settle(QUIET),
        Action::Keys(typed("when is my dentist appointment?")),
        Action::WaitContains("Bella: from what you told me".into()),
        // After that line, the next one naming Friday is her reply.
        Action::WaitContains("Friday".into()),
        Action::Settle(QUIET),
        Action::WaitSeen("ElephantHandler: said `Your dentist appointment is on Friday".into()),
    ]);
    leave_desktop(&mut steps);
    Interaction { steps }
}

fn bella_access_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    let say = |steps: &mut Vec<Action>, line: &str, wait: &str| {
        steps.push(Action::Keys(typed(line)));
        steps.push(Action::WaitContains(wait.into()));
        steps.push(Action::Settle(QUIET));
    };
    say(&mut steps, "info", "OtterHandler: info: home holds");
    say(
        &mut steps,
        "read notes",
        "OtterHandler: `notes` is `Documents/notes.txt` (found by name)",
    );
    say(&mut steps, "open Music", "Files: showing Home/Music");
    // The opened Files window has the keyboard now: back to Bella.
    let mut p = Ptr::start();
    steps.push(p.to(title_bar(ASSISTANT_WIN)));
    steps.push(Action::Button(true));
    steps.push(Action::Button(false));
    steps.push(Action::Settle(QUIET));
    say(
        &mut steps,
        "move Documents/notes.txt to Music",
        "OtterHandler: moved Documents/notes.txt to Music/notes.txt",
    );
    say(
        &mut steps,
        "undo",
        "OtterHandler: moved Music/notes.txt back to Documents/notes.txt",
    );
    // Trash asks first: `n` leaves it ...
    say(&mut steps, "trash recipes", "(found by name)");
    steps.push(Action::Keys(vec!["n"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::NeverSeen("to the Trash as".into()));
    // ... `y` moves it, into the Trash (Files can put it back).
    say(&mut steps, "trash recipes", "(found by name)");
    steps.push(Action::Keys(vec!["y"]));
    steps.push(Action::WaitContains(
        "OtterHandler: moved Documents/recipes to the Trash as .Trash/recipes".into(),
    ));
    steps.push(Action::Settle(QUIET));
    say(&mut steps, "launch Notes", "Bella: launched notes");
    steps.push(Action::WaitContains("Notes: opened".into()));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

fn bella_any_case_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("list music")));
    steps.push(Action::WaitContains(
        "OtterHandler: `music` is `Music`".into(),
    ));
    // Stage 1: the timing line (docs/stage1-design.md).
    steps.push(Action::WaitContains("ms by a typed command".into()));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("list musik")));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Screenshot("1-listed"));
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

fn check_any_case(img: &screen::Image) -> Result<String, String> {
    let at = content(img, "  file playlist.txt (46 bytes)")?;
    let miss = content(img, "Error: not found")?;
    Ok(format!(
        "`list music` listed Music's playlist.txt at {at:?}; `list musik` failed at {miss:?}"
    ))
}

fn memory_guard_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=put .bella-memory where is my music\\tlist Music",
        "ZOO_OS SHELL line=put .bella-learned where is my music\\tundo",
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=bella",
        "ZOO_OS PROC start pid=31 name=otter-handler",
        "ZOO_OS PROC start pid=32 name=elephant-handler",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC exit pid=31 name=otter-handler code=0",
        "ZOO_OS PROC exit pid=32 name=elephant-handler code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

fn memory_guard_steps() -> Interaction {
    let mut steps = vec![Action::WaitFor("ZOO_OS SHELL ready".into())];
    // What was learned before 0.116.0 had no confirmation: a sound
    // entry in the old file is set aside, never recalled
    // (docs/one-bella-design.md).
    steps.push(Action::Keys(typed(
        "put .bella-memory where is my music\\tlist Music",
    )));
    steps.push(Action::WaitContains("saved".into()));
    steps.push(Action::Settle(QUIET));
    // B4: a line planted in the memory file (any program with home can
    // write it), pairing a request with `undo`.
    steps.push(Action::Keys(typed(
        "put .bella-learned where is my music\\tundo",
    )));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("desktop")));
    steps.push(Action::WaitFor(
        "ZOO_OS PROC start pid=30 name=bella".into(),
    ));
    steps.push(Action::Settle(QUIET));
    // Files -> Crash Test -> Bella.
    steps.push(Action::Keys(vec!["tab", "tab"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("where is my music")));
    steps.push(Action::WaitContains(
        "OtterHandler: set aside .bella-memory as .bella-memory-unconfirmed".into(),
    ));
    steps.push(Action::WaitContains(
        "Bella: ignored remembered `undo`".into(),
    ));
    steps.push(Action::NeverSeen("recalled `list Music`".into()));
    steps.push(Action::Settle(QUIET));
    // B3: no model in this image, so the request stays unanswered; a
    // search that finds nothing must not answer it.
    steps.push(Action::Keys(typed("find my tax papers")));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("search taxes")));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::NeverSeen(
        "Bella: learned `find my tax papers`".into(),
    ));
    // B8: a learned `create Invoices` ...
    steps.push(Action::Keys(typed(
        "make a new project folder for work called Invoices",
    )));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(typed("create Invoices")));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["y"]));
    steps.push(Action::WaitContains(
        "Bella: learned `make a new project folder for work called Invoices` -> `create Invoices`"
            .into(),
    ));
    steps.push(Action::Settle(QUIET));
    // ... is not recalled for another folder's name, although the two
    // requests are 75% alike (6 of 8 words), enough to recall.
    steps.push(Action::Keys(typed(
        "make a new project folder for work called Receipts",
    )));
    steps.push(Action::WaitContains(
        "OtterHandler: `create Invoices` names other things than `make a new project folder for work called Receipts`"
            .into(),
    ));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::NeverSeen("Bella: recalled".into()));
    steps.push(Action::Keys(vec!["esc"]));
    steps.push(Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()));
    steps.push(Action::Settle(QUIET));
    Interaction { steps }
}

fn home_refresh_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=restart badger-handler after 15",
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
    ]);
    // The restart and the reload: process numbers depend on timing.
    e.then_rest();
    e
}

fn home_refresh_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = vec![
        Action::WaitFor("ZOO_OS SHELL ready".into()),
        Action::Keys(typed("restart badger-handler after 15")),
        Action::WaitContains("scheduled".into()),
    ];
    steps.extend(start_desktop().into_iter().skip(1));
    steps.extend([
        Action::WaitContains("Zookeeper: restarting badger-handler as scheduled".into()),
        Action::WaitContains("Zookeeper: sent the desktop session a fresh home folder".into()),
        Action::WaitContains(
            "Zookeeper: the filesystem restarted; reloading the desktop with a fresh home folder"
                .into(),
        ),
        // The new desktop's Files window.
        Action::WaitContains("PeacockHandler: animation open Files".into()),
        Action::Settle(QUIET),
        // Documents in Files' sidebar, through the new home folder.
        p.to(files_side(2)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("Files: showing Home/Documents".into()),
        Action::Settle(QUIET),
        // Files logs `showing` even when the listing failed.
        Action::NeverSeen("Files: cannot list".into()),
    ]);
    Interaction { steps }
}

/// The body the `net-http` test server sends; PuffinTest checks it.
const TEST_BODY: &str = "Hello from the host, ZooOS!";

/// A one-thread HTTP server on 127.0.0.1 for the `net-http` case: it
/// answers every request with `TEST_BODY` until stopped (bounded waits:
/// accept polls every 20 ms, reads every 200 ms, both until stopped).
struct TestServer {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: std::thread::JoinHandle<usize>,
}

impl TestServer {
    fn start() -> Result<TestServer, String> {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("test server: cannot listen: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("test server: {e}"))?
            .port();
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("test server: {e}"))?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut served = 0;
            while !flag.load(Ordering::Relaxed) {
                let Ok((mut conn, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                };
                // QEMU opens this connection when it starts (`guestfwd`
                // targets are character devices), long before the guest
                // sends anything: wait until the case ends, 200 ms at a
                // time.
                let _ = conn.set_nonblocking(false);
                let _ = conn.set_read_timeout(Some(Duration::from_millis(200)));
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") && !flag.load(Ordering::Relaxed) {
                    match conn.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => break,
                    }
                }
                if req.starts_with(b"GET ") {
                    let reply = format!(
                        "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                        TEST_BODY.len(),
                        TEST_BODY
                    );
                    let _ = conn.write_all(reply.as_bytes());
                    served += 1;
                }
                let _ = conn.shutdown(std::net::Shutdown::Both);
            }
            served
        });
        Ok(TestServer { port, stop, thread })
    }

    /// Stops the server; returns how many requests it answered.
    fn stop(self) -> usize {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        self.thread.join().unwrap_or(0)
    }
}

/// The `net-app` case: Network (pid 31) opens and lasts to the end.
fn network_app_expectation(normal: &Expectation) -> Expectation {
    let mut e = normal.clone();
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=26 name=zookeeper",
        "ZOO_OS PROC start pid=27 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=27 name=peacock-handler",
        "ZOO_OS PROC start pid=28 name=files",
        "ZOO_OS PROC start pid=29 name=crash-test",
        "ZOO_OS PROC start pid=30 name=bella",
        "ZOO_OS PROC start pid=31 name=network",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=31 name=network code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

fn network_app_steps() -> Interaction {
    let mut steps = start_desktop();
    // Files has the focus: Applications (last in Home), then Network
    // (the fifth application), opened with Enter.
    steps.push(Action::Keys(vec!["down"; 16]));
    steps.push(Action::Keys(vec!["ret"]));
    steps.push(Action::Settle(QUIET));
    steps.push(Action::Keys(vec!["down"; 4]));
    steps.push(Action::Keys(vec!["ret"]));
    steps.extend([
        Action::WaitFor("ZOO_OS PROC start pid=31 name=network".into()),
        Action::WaitContains("Network: connected 10.0.2.15 via 10.0.2.2".into()),
        Action::Settle(QUIET),
    ]);
    // Network has the focus: an address, and Enter.
    steps.push(Action::Keys(typed("10.0.2.100")));
    steps.extend([
        Action::WaitContains(
            "Network: fetched http://10.0.2.100/ (10.0.2.100): 92 bytes, `HTTP/1.0 200 OK`".into(),
        ),
        Action::Settle(QUIET),
        Action::Screenshot("1-fetched"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

fn check_network_app(img: &screen::Image) -> Result<String, String> {
    let t = title(img, "Network", true)?;
    let body = content(img, TEST_BODY)?;
    let status = ui(
        img,
        FaceId::Regular13,
        "Connected  10.0.2.15",
        LIGHT.syntax[5],
        LIGHT.content,
    )?;
    content(img, "Address: 10.0.2.100")?;
    Ok(format!(
        "`Network` focused {t:?}; `Connected  10.0.2.15` {status:?}; the server's text {body:?}"
    ))
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
            &format!("ZOO_OS PROC start pid={a} name=bella"),
            &format!("ZOO_OS PROC exit pid={p} name=peacock-handler code=0"),
            &format!("ZOO_OS DISPLAY released pid={p}"),
        ]);
        e.then_any_order(&[
            &format!("ZOO_OS PROC exit pid={f} name=files code=0"),
            &format!("ZOO_OS PROC exit pid={c} name=crash-test code=0"),
            &format!("ZOO_OS PROC exit pid={a} name=bella code=0"),
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

/// The centre-left of Files' content line `k` (first window, right of
/// its sidebar).
fn files_line(k: usize) -> (usize, usize) {
    finder_line(k)
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
        Action::WaitFor("ZOO_OS PROC start pid=35 name=bella".into()),
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
    let row = notes_band_row(y);
    let found = (x..x + zui::DOCK_ICON)
        .filter(|&px| notes_yellow(img.pixel(px, row)))
        .count();
    if found < zui::DOCK_ICON / 2 {
        return Err(format!(
            "no Notes yellow band in the fourth dock slot: {found} yellow pixels on row {row}"
        ));
    }
    Ok(format!("Notes back in the dock, in slot 4 at ({x}, {y})"))
}

/// The row through the yellow band at the top of the Notes icon whose
/// top is at `icon_y` (docs/u3g-design.md); no other icon is yellow there.
fn notes_band_row(icon_y: usize) -> usize {
    icon_y + zui::DOCK_ICON * 15 / 100
}

/// Notes' band yellow (a gradient from 0xFFE27A to 0xF6C12E).
fn notes_yellow(p: Option<[u8; 3]>) -> bool {
    p.is_some_and(|[r, g, b]| r >= 0xF0 && (0xB8..=0xE8).contains(&g) && b <= 0x90)
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
        "ZOO_OS PROC start pid=30 name=bella",
        "ZOO_OS PROC start pid=31 name=files",
        "ZOO_OS PROC exit pid=31 name=files code=0",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
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
        "ZOO_OS PROC start pid=30 name=bella",
        "ZOO_OS PROC exit pid=29 name=crash-test code=0",
        "ZOO_OS PROC start pid=31 name=crash-test",
        "ZOO_OS PROC exit pid=27 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=27",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=28 name=files code=0",
        "ZOO_OS PROC exit pid=31 name=crash-test code=0",
        "ZOO_OS PROC exit pid=30 name=bella code=0",
        "ZOO_OS PROC exit pid=26 name=zookeeper code=0",
    ]);
    e
}

/// A running dot under dock icon `k` (4 px, the menu-bar text colour).
fn dock_dot(img: &screen::Image, k: usize) -> bool {
    let (x, y) = zui::dock_icon(SCREEN.0, SCREEN.1, 4, k);
    img.pixel(x + zui::DOCK_ICON / 2 - 1, y + zui::DOCK_ICON + 3) == Some(rgb(LIGHT.bar_text))
}

/// The running dots' centres on the dock's dot row. With the pointer on
/// the dock, icons are magnified and move (docs/u2g-design.md), so hover
/// checks count dots and look under the pointer, where the hovered icon
/// stays.
fn dock_dots(img: &screen::Image) -> Vec<usize> {
    let (x, y, w, _) = zui::dock_rect(SCREEN.0, SCREEN.1, 4);
    let row = y + zui::DOCK_PAD + zui::DOCK_ICON + 3;
    let mut out: Vec<usize> = Vec::new();
    for px in x.saturating_sub(80)..x + w + 80 {
        if img.pixel(px, row) == Some(rgb(LIGHT.bar_text))
            && out.last().is_none_or(|&last| px > last + 6)
        {
            out.push(px);
        }
    }
    out
}

fn dock_magnify_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        p.to((dock_centre(1).0, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        // Onto Crash Test's icon: it grows.
        p.to(dock_centre(1)),
        Action::Settle(QUIET),
        Action::Screenshot("1-magnified"),
        // Down to the very bottom edge: the dock stays, the icons settle.
        p.to((dock_centre(1).0, SCREEN.1 - 1)),
        Action::Settle(QUIET),
        Action::Screenshot("2-settled"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

/// Crash Test's grey tile (its gradient), not the wallpaper.
fn tile_grey(p: Option<[u8; 3]>) -> bool {
    p.is_some_and(|[r, g, b]| r.abs_diff(g) < 14 && g.abs_diff(b) < 16 && (100..=215).contains(&r))
}

/// How far above the dock's top edge the icon at x reaches (its tile's
/// highest row), in px.
fn reach_above_dock(img: &screen::Image, x: usize) -> usize {
    let (_, dy, _, _) = zui::dock_rect(SCREEN.0, SCREEN.1, 4);
    (1..40)
        .take_while(|&k| tile_grey(img.pixel(x, dy - k)))
        .count()
}

/// A column through Crash Test's tile, left of its warning sign, inside
/// the icon at rest and magnified.
fn crash_column() -> usize {
    dock_centre(1).0 - 22
}

fn check_magnified(img: &screen::Image) -> Result<String, String> {
    let x = crash_column();
    let up = reach_above_dock(img, x);
    tooltip(img, "Crash Test")?;
    // Fully magnified: 84 px against 52, so 32 px above where it rests,
    // which is 8 px below the dock's top: 24 px above the dock.
    if up < 18 {
        return Err(format!(
            "Crash Test's icon reaches only {up} px above the dock; magnified it should reach about 24"
        ));
    }
    Ok(format!(
        "pointer on Crash Test: its icon reaches {up} px above the dock (magnified)"
    ))
}

fn check_settled(img: &screen::Image) -> Result<String, String> {
    let x = crash_column();
    let up = reach_above_dock(img, x);
    if up > 0 {
        return Err(format!(
            "Crash Test's icon still reaches {up} px above the dock after the pointer left the icons"
        ));
    }
    if !dock_dot(img, 0) || !dock_dot(img, 1) || !dock_dot(img, 2) {
        return Err("settled dock: dots not back at their places".into());
    }
    Ok("pointer below the icons: they are back at their size and places".into())
}

/// Menu-bar title `k`'s span (0 the mark, 1 the application name, then
/// File, Edit, View, Window, Help), as PeacockHandler lays them out with
/// application `app` focused (docs/u4a-design.md).
fn bar_title(app: &str, k: usize) -> (usize, usize) {
    let mut x = 40;
    let end = x + screen::ui_width(FaceId::SemiBold13, app);
    if k == 1 {
        return (x - 8, end + 8);
    }
    x = end + 22;
    for (j, m) in ["File", "Edit", "View", "Window", "Help"]
        .iter()
        .enumerate()
    {
        let end = x + screen::ui_width(FaceId::Regular13, m);
        if j + 2 == k {
            return (x - 8, end + 8);
        }
        x = end + 20;
    }
    (5, 31)
}

fn bar_centre(app: &str, k: usize) -> (usize, usize) {
    let (a, b) = bar_title(app, k);
    ((a + b) / 2, zui::MENU_H / 2)
}

/// A point on item `k` of the menu hanging from bar title `t` (separators
/// as `seps` says).
fn bar_item(app: &str, t: usize, seps: &[bool], k: usize) -> (usize, usize) {
    let (a, _) = bar_title(app, t);
    // Right of the labels, so the pointer does not cover them.
    (
        a + 170,
        zui::menu_item_top(zui::MENU_H + 2, seps, k) + zui::MENU_ITEM_H / 2,
    )
}

fn menu_bar_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    // Minimize, Zoom, a line, then the windows.
    let window_seps = [false, false, true, false, false, false];
    steps.extend([
        p.to(bar_centre("Files", 5)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains(
            "PeacockHandler: menu bar Window: Minimize, Zoom, Files, Crash Test, Bella".into(),
        ),
        p.to(bar_item("Files", 5, &window_seps, 5)),
        Action::Settle(QUIET),
        Action::Screenshot("1-window-menu"),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: brought Bella forward".into()),
        // File's menu, then along the bar to View: the menus switch.
        p.to(bar_centre("Bella", 2)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: menu bar File: New Window, Close Window".into()),
        p.to(bar_centre("Bella", 4)),
        Action::WaitContains("PeacockHandler: menu bar View: Dark Appearance".into()),
        p.to(bar_item("Bella", 4, &[false], 0)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("PeacockHandler: appearance dark".into()),
        // Ctrl+M: the Bella to the dock.
        Action::Key("ctrl", true),
        Action::Keys(vec!["m"]),
        Action::Key("ctrl", false),
        Action::WaitContains("PeacockHandler: animation minimize Bella".into()),
        p.to((SCREEN.0 / 2, SCREEN.1 / 2)),
        Action::Settle(QUIET),
        Action::Screenshot("2-after"),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

fn check_window_menu(img: &screen::Image) -> Result<String, String> {
    let min = ui(
        img,
        FaceId::Regular13,
        "Minimize",
        LIGHT.tooltip_text,
        LIGHT.tooltip,
    )?;
    let keys = ui(
        img,
        FaceId::Regular13,
        "Ctrl+M",
        LIGHT.text_dim,
        LIGHT.tooltip,
    )?;
    let crash = ui(
        img,
        FaceId::Regular13,
        "Crash Test",
        LIGHT.tooltip_text,
        LIGHT.tooltip,
    )?;
    // The pointer is on the Bella item: highlighted.
    let on = ui(
        img,
        FaceId::Regular13,
        "Bella",
        LIGHT.select_text,
        LIGHT.select,
    )?;
    if keys.1 != min.1 || keys.0 <= min.0 {
        return Err(format!(
            "`Ctrl+M` at {keys:?} should be right of `Minimize` at {min:?}, on its line"
        ));
    }
    Ok(format!(
        "the Window menu: `Minimize` {min:?} with `Ctrl+M` {keys:?}, `Crash Test` {crash:?}, `Bella` highlighted {on:?}"
    ))
}

fn check_menu_after(img: &screen::Image) -> Result<String, String> {
    let d = &zui::DARK;
    // Dark appearance, the Bella gone to the dock, Files focused again.
    let files = ui(img, FaceId::Medium13, "Files", d.title_text, d.title)?;
    no_ui(img, FaceId::Medium13, "Bella", d.title_text, d.title)?;
    no_ui(
        img,
        FaceId::Medium13,
        "Bella",
        d.title_text_blurred,
        d.title_blurred,
    )?;
    Ok(format!(
        "dark appearance; the Bella minimised; `Files` focused at {files:?}"
    ))
}

fn check_dark_again(img: &screen::Image) -> Result<String, String> {
    let d = &zui::DARK;
    let t = ui(img, FaceId::Medium13, "Files", d.title_text, d.title)?;
    no_title(img, "Files", true)?;
    Ok(format!(
        "after a restart the desktop is dark: `Files` title in dark colours at {t:?}"
    ))
}

fn dock_bounce_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        p.to((dock_centre(3).0, SCREEN.1 - 1)),
        Action::Settle(DOCK_SLIDE),
        p.to(dock_centre(3)),
        // Shift held: 6.4 s instead of 0.64 s.
        Action::Key("shift", true),
        Action::Button(true),
        Action::Button(false),
        Action::Key("shift", false),
        Action::WaitContains("PeacockHandler: opened Notes".into()),
        // Off the icons (no magnification); the dock stays while it hops.
        p.to((dock_centre(3).0, SCREEN.1 - 1)),
        // About 1.2 s in: near the top of the first hop.
        Action::Settle(QUIET),
        Action::Screenshot("1-in-the-air"),
        animation_done("bounce Notes", 20, (6000, 8000)),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

/// Notes' icon in the air: its yellow band is on a row above the icon's
/// resting top, where at rest there is only wallpaper.
fn check_bounce(img: &screen::Image) -> Result<String, String> {
    let (x, y) = zui::dock_icon(SCREEN.0, SCREEN.1, 4, 3);
    let row = y - 14;
    let found = (x..x + zui::DOCK_ICON)
        .filter(|&px| notes_yellow(img.pixel(px, row)))
        .count();
    if found < zui::DOCK_ICON / 2 {
        return Err(format!(
            "Notes' icon is not in the air: {found} yellow pixels on row {row}, 14 px above its resting top"
        ));
    }
    Ok(format!(
        "Notes' icon in the air: its yellow band on row {row}, above its resting top ({found} pixels)"
    ))
}

/// Opens the context menu on Files' content line `line` and chooses item
/// `k` of `n` (no separators).
fn files_menu(p: &mut Ptr, line: usize, menu: &str, n: usize, k: usize) -> Vec<Action> {
    let (x, y) = finder_line(line);
    let seps = vec![false; n];
    vec![
        p.to((x, y)),
        Action::RightButton(true),
        Action::RightButton(false),
        Action::WaitContains(format!("PeacockHandler: menu Files: {menu}")),
        p.to((
            x + 130,
            zui::menu_item_top(y + 2, &seps, k) + zui::MENU_ITEM_H / 2,
        )),
        Action::Button(true),
        Action::Button(false),
    ]
}

fn files_manage_steps() -> Interaction {
    let mut p = Ptr::start();
    let mut steps = start_desktop();
    steps.extend([
        p.to(files_side(2)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("Files: showing Home/Documents".into()),
    ]);
    // notes.txt: Rename, the name cleared and typed.
    steps.extend(files_menu(
        &mut p,
        1,
        "Open in Notes, Quick Look, Rename, Move to Trash",
        4,
        2,
    ));
    // The keys only once renaming has begun (a test types faster than
    // the menu's answer reaches Files).
    steps.push(Action::WaitContains("Files: renaming notes.txt".into()));
    steps.push(Action::Keys(vec!["backspace"; 9]));
    steps.push(Action::Keys(typed("shopping.txt")));
    steps.push(Action::WaitContains(
        "Files: renamed Documents/notes.txt to Documents/shopping.txt".into(),
    ));
    // recipes (first now): Move to Trash.
    steps.extend(files_menu(
        &mut p,
        1,
        "Open, New Folder, Rename, Move to Trash",
        4,
        3,
    ));
    steps.extend([
        Action::WaitContains("Files: moved Documents/recipes to the Trash".into()),
        p.to(files_side(6)),
        Action::Button(true),
        Action::Button(false),
        Action::WaitContains("Files: showing Home/.Trash".into()),
        p.to((SCREEN.0 / 2, SCREEN.1 / 2)),
        Action::Settle(QUIET),
        Action::Screenshot("1-trash"),
    ]);
    // Put Back: into Home.
    steps.extend(files_menu(&mut p, 1, "Put Back", 1, 0));
    steps.extend([
        Action::WaitContains("Files: put recipes back in Home".into()),
        Action::Keys(vec!["esc"]),
        Action::WaitFor("ZOO_OS DISPLAY released pid=27".into()),
    ]);
    Interaction { steps }
}

fn check_trash(img: &screen::Image) -> Result<String, String> {
    let place = content(img, "Trash")?;
    let item = selected(img, "recipes", true)?;
    no_content(img, "Home/.Trash")?;
    Ok(format!(
        "the Trash: its location `Trash` at {place:?}, `recipes` in it at {item:?}"
    ))
}

/// Whether one of `dots` is under x (within 6 px).
fn dot_at(dots: &[usize], x: usize) -> bool {
    dots.iter().any(|&d| d.abs_diff(x) <= 6)
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
    let dots = dock_dots(img);
    if dots.len() != 3 || !dot_at(&dots, dock_centre(0).0) {
        return Err(format!(
            "running dots should mark the first three apps only, one under the Files icon at x {}: {dots:?}",
            dock_centre(0).0
        ));
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
    let dots = dock_dots(img);
    if dots.len() != 2 || dot_at(&dots, dock_centre(1).0) || dots[0] >= dock_centre(1).0 {
        return Err(format!(
            "dots should mark Files and the Bella only, none under Crash Test at x {}: {dots:?}",
            dock_centre(1).0
        ));
    }
    Ok("dock: Crash Test's name shown, no running dot under it, dots under Files and Bella".into())
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
        Action::WaitContains("PeacockHandler: animation minimize Bella".into()),
        // The dock up (the pointer at the bottom edge; it may still be up
        // from the genie).
        p.to((SCREEN.0 / 2 - 300, SCREEN.1 - 1)),
        Action::Settle(QUIET),
        Action::Screenshot("1-minis"),
        // Files' thumbnail (the first): Files comes back, the Bella
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
/// with their applications' badges: Files' blue first, the Bella's
/// violet second.
fn check_dock_minis(img: &screen::Image) -> Result<String, String> {
    let (_, y, _, h) = zui::dock_rect_with(SCREEN.0, SCREEN.1, 4, 2);
    let lx = zui::dock_divider_x(SCREEN.0, SCREEN.1, 4, 2);
    if img.pixel(lx, y + h / 2) != Some(rgb(LIGHT.separator)) {
        return Err(format!(
            "no divider at x {lx}: {:?}",
            img.pixel(lx, y + h / 2)
        ));
    }
    for (j, (c, name)) in [(0x3B8EEA, "Files"), (0x7A5AF8, "Bella")]
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
        "the divider at x {lx}, then thumbnails of Files and the Bella with their badges"
    ))
}

fn check_files_back(img: &screen::Image) -> Result<String, String> {
    let t = title(img, "Files", true)?;
    no_title(img, "Bella", false)?;
    no_title(img, "Bella", true)?;
    Ok(format!(
        "Files back and focused at {t:?}; the Bella still in the dock"
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
        Action::WaitContains("PeacockHandler: new window Files (window 5)".into()),
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
    let row = notes_band_row(y + zui::DOCK_PAD);
    let found = (x..x + w)
        .filter(|&px| notes_yellow(img.pixel(px, row)))
        .count();
    if found > 2 {
        return Err(format!(
            "Notes' yellow band is still in the dock ({found} pixels on row {row})"
        ));
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
        Action::WaitContains("PeacockHandler: new window Files (window 5)".into()),
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
    let want = title_pen(moved, "Bella");
    let at = title(img, "Bella", true)?;
    if at != want {
        return Err(format!(
            "`Bella` title at {at:?}, expected {want:?} after the drag"
        ));
    }
    no_title(img, "Crash Test", true)?;
    Ok(format!(
        "`Bella` title moved from {:?} to {at:?} and focused",
        title_pen(w, "Bella")
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
    let first = selected(img, "Documents", true)?;
    content(img, "This window is a crash test.")?;
    title(img, "Bella", false)?;
    no_selected(img, "Documentsx", true)?;
    no_title(img, "Files", false)?;
    Ok(format!(
        "menu bar names `Files` at {menu:?}; `Files` focused (title at {t:?}) with `Documents` selected at {first:?}; Crash Test and the Bella shown; wrong text rejected"
    ))
}

fn check_file_details(img: &screen::Image) -> Result<String, String> {
    let name = content(img, "Home/Photos")?;
    selected(img, "README.txt", true)?;
    let size = selected(img, "34 bytes", true)?;
    no_content(img, "Home/Music")?;
    Ok(format!(
        "3rd entry opened: `Home/Photos` at {name:?}, `README.txt` selected, its size `34 bytes` at {size:?}; wrong folder rejected"
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
    let first = selected(img, "Documents", true)?;
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
            '?' => "shift+slash",
            '\\' => "backslash",
            '\'' => "apostrophe",
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
