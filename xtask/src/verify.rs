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
    pub timeout: Duration,
    pub negative_timeout: Duration,
    pub settle: Duration,
}

/// Inspects a screenshot; `Ok` carries a summary, `Err` why it failed.
type ScreenCheck = fn(&screen::Image) -> Result<String, String>;

struct Case {
    name: &'static str,
    mode: Mode,
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
    "ZOO_OS PROC start pid=2 name=lion-handler",
    "ZOO_OS PROC start pid=3 name=monkey-handler",
    "ZOO_OS PROC crash pid=3 name=monkey-handler fault=page-fault access=read cause=protection addr=0xffffc00000001000",
    "ZOO_OS PROC start pid=4 name=monkey-handler",
    "ZOO_OS PROC crash pid=4 name=monkey-handler fault=page-fault access=read cause=protection addr=0xffffc00000001000",
    "ZOO_OS PROC start pid=5 name=hamster-handler",
    "ZOO_OS PROC killed pid=5 name=hamster-handler by=1",
];

/// M5a: two BeaverHandlers start in order and finish concurrently.
const BEAVERS_START: [&str; 2] = [
    "ZOO_OS PROC start pid=6 name=beaver-handler",
    "ZOO_OS PROC start pid=7 name=beaver-handler",
];
const BEAVERS_EXIT: [&str; 2] = [
    "ZOO_OS PROC exit pid=6 name=beaver-handler code=0",
    "ZOO_OS PROC exit pid=7 name=beaver-handler code=0",
];
const BOOT_PROCESSES_END: [&str; 5] = [
    "ZOO_OS PROC exit pid=2 name=lion-handler code=0",
    "ZOO_OS PROC exit pid=1 name=zookeeper code=0",
    "ZOO_OS TEST process-cleanup ok",
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

    let cases = [
        Case {
            name: "normal",
            mode: Mode::Normal,
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
            name: "negative-wrong-marker",
            mode: Mode::Panic,
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
    for c in &cases {
        let img = &images
            .iter()
            .find(|(m, _)| *m == c.mode)
            .expect("every mode was built")
            .1;
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
    // 1. Start the session.
    e.then(&[
        "ZOO_OS SHELL line=desktop",
        "ZOO_OS PROC start pid=8 name=zookeeper",
        "ZOO_OS PROC start pid=9 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=9 name=peacock-handler",
        "ZOO_OS PROC start pid=10 name=owl-handler",
        "ZOO_OS PROC start pid=11 name=chimp-handler",
    ]);
    // 3. ChimpHandler crashes; the window server restarts it.
    let chimp_crash =
        format!("ZOO_OS PROC crash pid=11 name=chimp-handler {PAGE_FAULT_KERNEL_READ}");
    e.then(&[&chimp_crash, "ZOO_OS PROC start pid=12 name=chimp-handler"]);
    // 4. The window server crashes; its apps end while Zookeeper restarts it.
    let server_crash =
        format!("ZOO_OS PROC crash pid=9 name=peacock-handler {PAGE_FAULT_KERNEL_READ}");
    e.then(&[&server_crash, "ZOO_OS DISPLAY released pid=9"]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=10 name=owl-handler code=0",
        "ZOO_OS PROC exit pid=12 name=chimp-handler code=0",
        "ZOO_OS PROC start pid=13 name=peacock-handler",
        "ZOO_OS DISPLAY acquired pid=13 name=peacock-handler",
        "ZOO_OS PROC start pid=14 name=owl-handler",
        "ZOO_OS PROC start pid=15 name=chimp-handler",
    ]);
    // 5. Escape ends the session cleanly; the kernel shell is back.
    e.then(&[
        "ZOO_OS PROC exit pid=13 name=peacock-handler code=0",
        "ZOO_OS DISPLAY released pid=13",
    ]);
    e.then_any_order(&[
        "ZOO_OS PROC exit pid=14 name=owl-handler code=0",
        "ZOO_OS PROC exit pid=15 name=chimp-handler code=0",
        "ZOO_OS PROC exit pid=8 name=zookeeper code=0",
    ]);
    e.then(&["ZOO_OS SHELL line=echo back"]);
    e.clone()
}

fn desktop_steps() -> Interaction {
    Interaction {
        steps: vec![
            Action::WaitFor("ZOO_OS SHELL ready".into()),
            Action::Keys(vec!["d", "e", "s", "k", "t", "o", "p", "ret"]),
            Action::WaitFor("ZOO_OS PROC start pid=11 name=chimp-handler".into()),
            Action::Settle(QUIET),
            Action::Screenshot("1-desktop"),
            // Files: down twice, open the details view.
            Action::Keys(vec!["down", "down", "ret"]),
            Action::Settle(QUIET),
            Action::Screenshot("2-details"),
            // Focus Chimp and make it crash.
            Action::Keys(vec!["tab", "ret"]),
            Action::WaitFor("ZOO_OS PROC start pid=12 name=chimp-handler".into()),
            Action::Settle(QUIET),
            Action::Screenshot("3-app-restart"),
            // Test key: crash the window server itself.
            Action::Keys(vec!["f12"]),
            Action::WaitFor("ZOO_OS PROC start pid=15 name=chimp-handler".into()),
            Action::Settle(QUIET),
            Action::Screenshot("4-server-restart"),
            // Leave the desktop and use the kernel shell again.
            Action::Keys(vec!["esc"]),
            Action::WaitFor("ZOO_OS DISPLAY released pid=13".into()),
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
    need(img, "ChimpHandler is a crash test.", CONTENT_TEXT, CONTENT)?;
    absent(img, "bin/zookeeperx", CONTENT_TEXT, SELECTED)?;
    absent(img, "Files", BAR_TEXT, DESKTOP_BG)?;
    Ok(format!(
        "menu `ZooOS` at {menu:?}, focused title `Files` at {title:?}, selected `bin/zookeeper` at {first:?}, Chimp window shown; wrong text rejected"
    ))
}

fn check_file_details(img: &screen::Image) -> Result<String, String> {
    let name = need(img, "Name: bin/monkey-handler", CONTENT_TEXT, CONTENT)?;
    let size = need(img, "Size: ", CONTENT_TEXT, CONTENT)?;
    absent(img, "Name: bin/zookeeper", CONTENT_TEXT, CONTENT)?;
    Ok(format!(
        "details of the 3rd file: `Name: bin/monkey-handler` at {name:?}, `Size:` at {size:?}; wrong file rejected"
    ))
}

fn check_app_restart(img: &screen::Image) -> Result<String, String> {
    let status = need(
        img,
        "ChimpHandler crashed; restarted (1 of 3)",
        BAR_TEXT,
        BAR,
    )?;
    let title = need(img, "Chimp", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    need(img, "ChimpHandler is a crash test.", CONTENT_TEXT, CONTENT)?;
    // Files kept its state through Chimp's crash.
    need(img, "Name: bin/monkey-handler", CONTENT_TEXT, CONTENT)?;
    absent(
        img,
        "ChimpHandler crashed; restarted (2 of 3)",
        BAR_TEXT,
        BAR,
    )?;
    Ok(format!(
        "status `ChimpHandler crashed; restarted (1 of 3)` at {status:?}, focused `Chimp` at {title:?}, Files details unchanged"
    ))
}

fn check_server_restart(img: &screen::Image) -> Result<String, String> {
    let menu = need(img, "ZooOS", BAR_TEXT, BAR)?;
    let title = need(img, "Files", TITLE_FOCUSED_TEXT, TITLE_FOCUSED)?;
    let first = need(img, "bin/zookeeper", CONTENT_TEXT, SELECTED)?;
    need(img, "Tab: next window", BAR_TEXT, BAR)?;
    // A fresh window server: the old crash notice is gone.
    absent(
        img,
        "ChimpHandler crashed; restarted (1 of 3)",
        BAR_TEXT,
        BAR,
    )?;
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
