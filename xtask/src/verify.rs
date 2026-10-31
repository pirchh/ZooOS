//! `cargo xtask verify`: positive and negative QEMU boot cases.

use crate::config::Tools;
use crate::image::{self, Mode};
use crate::markers::Expectation;
use crate::qemu::{self, Action, Interaction, Outcome};
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

struct Case {
    name: &'static str,
    mode: Mode,
    /// Boot this image instead of the mode's own (the model image).
    custom_image: Option<std::path::PathBuf>,
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
    let mut normal = expect(Mode::Normal, &head);
    normal
        .then_any_order(&BEAVERS_EXIT)
        .then(&BADGER_START)
        .then(&ARCTICFOX)
        .then(&OTTER)
        .then(&BADGER_EXIT)
        .then(&ANT)
        .then(&MOLE)
        .then(&GECKO)
        .then(&PTARMIGAN)
        .then(&BOOT_PROCESSES_END);
    let mut keyboard = normal.clone();
    keyboard.then(&["ZOO_OS SHELL line=echo hi", "ZOO_OS SHELL line=mem"]);
    let mut scroll = normal.clone();
    scroll
        .then(&["ZOO_OS SHELL line=help"; 4])
        .then(&["ZOO_OS SHELL line=echo end"]);
    let mut never = normal.clone();
    never.then(&["ZOO_OS NEVER-EMITTED"]);
    let desktop = desktop_expectation(&normal);
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
            expectation: normal.clone(),
            timeout: s.timeout,
            want: "pass",
            purpose: "self-tests, the Zookeeper process scenario, READY; no PANIC or EXCEPTION",
            interaction: None,
            screen_checks: vec![],
        },
        Case {
            name: "panic",
            mode: Mode::Panic,
            custom_image: None,
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
            name: "assistant",
            mode: Mode::Normal,
            custom_image: None,
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
            expectation: services_expectation(&normal),
            timeout: s.timeout * 3,
            want: "pass",
            purpose: "resident supervisor: services listed, crash and hang detected and restarted, restart limit, logs",
            interaction: Some(services_steps()),
            screen_checks: vec![],
        },
        Case {
            name: "c-program",
            mode: Mode::Normal,
            custom_image: None,
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
        )?;
        let mut kind = r.outcome.kind().to_string();
        let mut detail = r.outcome.detail().to_string();
        if kind == "pass" {
            for (name, check) in &c.screen_checks {
                let Some((_, ppm)) = r.screenshots.iter().find(|(n, _)| n == name) else {
                    kind = "harness-error".into();
                    detail = format!("screenshot `{name}` was not taken");
                    break;
                };
                match screen::read_ppm(ppm).and_then(|img| check(&img)) {
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
    let sel = need(img, "bin/lemming-test", CONTENT_TEXT, SELECTED)?;
    absent(img, "bin/zookeeper", CONTENT_TEXT, SELECTED)?;
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
    let first = need(img, "bin/zookeeper", CONTENT_TEXT, SELECTED)?;
    need(img, "This window is a crash test.", CONTENT_TEXT, CONTENT)?;
    absent(img, "bin/zookeeperx", CONTENT_TEXT, SELECTED)?;
    absent(img, "Files", BAR_TEXT, DESKTOP_BG)?;
    Ok(format!(
        "menu `ZooOS` at {menu:?}, focused title `Files` at {title:?}, selected `bin/zookeeper` at {first:?}, Crash Test window shown; wrong text rejected"
    ))
}

fn check_file_details(img: &screen::Image) -> Result<String, String> {
    let name = need(img, "Name: bin/lemming-test", CONTENT_TEXT, CONTENT)?;
    let size = need(img, "Size: ", CONTENT_TEXT, CONTENT)?;
    absent(img, "Name: bin/zookeeper", CONTENT_TEXT, CONTENT)?;
    Ok(format!(
        "details of the 3rd file: `Name: bin/lemming-test` at {name:?}, `Size:` at {size:?}; wrong file rejected"
    ))
}

fn check_app_restart(img: &screen::Image) -> Result<String, String> {
    let status = need(img, "Crash Test crashed; restarted (1 of 3)", BAR_TEXT, BAR)?;
    let title = need(img, "Crash Test", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    need(img, "This window is a crash test.", CONTENT_TEXT, CONTENT)?;
    // Files kept its state through Crash Test's crash.
    need(img, "Name: bin/lemming-test", CONTENT_TEXT, CONTENT)?;
    absent(img, "Crash Test crashed; restarted (2 of 3)", BAR_TEXT, BAR)?;
    Ok(format!(
        "status `Crash Test crashed; restarted (1 of 3)` at {status:?}, focused `Crash Test` at {title:?}, Files details unchanged"
    ))
}

fn check_server_restart(img: &screen::Image) -> Result<String, String> {
    let menu = need(img, "ZooOS", BAR_TEXT, BAR)?;
    let title = need(img, "Files", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    let first = need(img, "bin/zookeeper", CONTENT_TEXT, SELECTED)?;
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
