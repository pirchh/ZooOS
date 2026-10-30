//! `cargo xtask verify`: positive and negative QEMU boot cases.

use crate::config::Tools;
use crate::image::{self, Mode};
use crate::markers::{Expectation, Matcher};
use crate::qemu::{self, Interaction, Outcome};
use crate::screen;
use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

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
    /// Check applied to the screenshot; failing it fails the case.
    screen_check: Option<ScreenCheck>,
}

const SELF_TESTS: [&str; 5] = [
    "ORIGINS_OS TEST breakpoint ok",
    "ORIGINS_OS TEST frames ok",
    "ORIGINS_OS TEST paging ok",
    "ORIGINS_OS TEST heap ok",
    "ORIGINS_OS TEST timer ok",
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

    let mut normal_tail = with_tests("ORIGINS_OS READY");
    normal_tail.push("ORIGINS_OS SHELL ready");
    let normal = expect(Mode::Normal, &normal_tail);
    let mut keyboard_tail = normal_tail.clone();
    keyboard_tail.extend(["ORIGINS_OS SHELL line=echo hi", "ORIGINS_OS SHELL line=mem"]);
    let mut scroll_tail = normal_tail.clone();
    scroll_tail.extend(["ORIGINS_OS SHELL line=help"; 4]);
    scroll_tail.push("ORIGINS_OS SHELL line=echo end");
    let mut never = normal.clone();
    never
        .required
        .push(Matcher::Exact("ORIGINS_OS NEVER-EMITTED".into()));

    let cases = [
        Case {
            name: "normal",
            mode: Mode::Normal,
            expectation: normal.clone(),
            timeout: s.timeout,
            want: "pass",
            purpose: "BOOT, INFO, five self-tests, READY; no PANIC or EXCEPTION",
            interaction: None,
            screen_check: None,
        },
        Case {
            name: "panic",
            mode: Mode::Panic,
            expectation: expect(
                Mode::Panic,
                &with_tests("ORIGINS_OS PANIC intentional-test"),
            ),
            timeout: s.timeout,
            want: "pass",
            purpose: "self-tests, then PANIC with interrupts on; no READY",
            interaction: None,
            screen_check: None,
        },
        Case {
            name: "fault-write-protect",
            mode: Mode::FaultWriteProtect,
            expectation: expect(
                Mode::FaultWriteProtect,
                &[
                    "ORIGINS_OS EXCEPTION page-fault access=write cause=protection addr=0xffffc00040000000",
                ],
            ),
            timeout: s.timeout,
            want: "pass",
            purpose: "write to a read-only page is reported as a protection fault",
            interaction: None,
            screen_check: None,
        },
        Case {
            name: "fault-heap-guard",
            mode: Mode::FaultHeapGuard,
            expectation: expect(
                Mode::FaultHeapGuard,
                &[
                    "ORIGINS_OS EXCEPTION page-fault access=read cause=not-present addr=0xffffc00000101000",
                ],
            ),
            timeout: s.timeout,
            want: "pass",
            purpose: "read of the heap guard page is reported as not-present",
            interaction: None,
            screen_check: None,
        },
        Case {
            name: "fault-stack-overflow",
            mode: Mode::FaultStackOverflow,
            expectation: expect(
                Mode::FaultStackOverflow,
                &["ORIGINS_OS EXCEPTION double-fault stack-guard-hit"],
            ),
            timeout: s.timeout,
            want: "pass",
            purpose: "stack overflow hits the guard page and is reported via the IST stack",
            interaction: None,
            screen_check: None,
        },
        Case {
            name: "keyboard",
            mode: Mode::Normal,
            expectation: expect(Mode::Normal, &keyboard_tail),
            timeout: s.timeout,
            want: "pass",
            purpose: "keys typed via QMP reach the shell (with Backspace); screen shows the text",
            interaction: Some(Interaction {
                trigger: "ORIGINS_OS SHELL ready".into(),
                // "echx", Backspace, "o hi", Enter, then "mem", Enter.
                keys: vec![
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
                ],
                screenshot: true,
            }),
            screen_check: Some(check_boot_and_echo),
        },
        Case {
            name: "scroll",
            mode: Mode::Normal,
            expectation: expect(Mode::Normal, &scroll_tail),
            timeout: s.timeout,
            want: "pass",
            purpose: "four `help` outputs overflow the screen; the console scrolls correctly",
            interaction: Some(Interaction {
                trigger: "ORIGINS_OS SHELL ready".into(),
                keys: scroll_keys(),
                screenshot: true,
            }),
            screen_check: Some(check_scrolled),
        },
        Case {
            name: "negative-wrong-marker",
            mode: Mode::Panic,
            expectation: normal.clone(),
            timeout: s.timeout,
            want: "unexpected-marker",
            purpose: "verifier must reject the panic image under normal expectations",
            interaction: None,
            screen_check: None,
        },
        Case {
            name: "negative-timeout",
            mode: Mode::Normal,
            expectation: never,
            timeout: s.negative_timeout,
            want: "timeout",
            purpose: "verifier must reject a marker that never appears, by timeout",
            interaction: None,
            screen_check: None,
        },
        Case {
            name: "negative-triple-fault",
            mode: Mode::TripleFault,
            expectation: expect(Mode::TripleFault, &["ORIGINS_OS READY"]),
            timeout: s.timeout,
            want: "early-exit",
            purpose: "verifier must reject a guest that triple-faults (reset with -no-reboot)",
            interaction: None,
            screen_check: None,
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
        if let (Some(ppm), Some(check)) = (&r.screenshot, c.screen_check) {
            match screen::read_ppm(ppm).and_then(|img| check(&img)) {
                Ok(summary) => detail = format!("{detail}; screen: {summary}"),
                Err(why) => {
                    kind = "screen-mismatch".into();
                    detail = format!("screen: {why} ({})", ppm.display());
                }
            }
        } else if c.interaction.as_ref().is_some_and(|i| i.screenshot) && kind == "pass" {
            kind = "harness-error".into();
            detail = "no screenshot was taken".into();
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
fn check_boot_and_echo(img: &screen::Image) -> Result<String, String> {
    let boot = format!("ORIGINS_OS BOOT version={VERSION}");
    if !screen::row_matches(img, &boot, 0) {
        return Err(format!("row 0 does not show `{boot}`"));
    }
    // The same comparison must reject a wrong string, or it proves nothing.
    if screen::row_matches(img, "ORIGINS_OS BOOT version=9.9.9", 0) {
        return Err("row 0 also matches a wrong version; the check is not discriminating".into());
    }
    let row = screen::find_row(img, "origins> echo hi").ok_or("no row shows `origins> echo hi`")?;
    if !screen::row_matches(img, "hi", row + 1) {
        return Err(format!(
            "row {} below the command does not show `hi`",
            row + 1
        ));
    }
    Ok(format!(
        "{}x{}, {} lit pixels; row 0 = BOOT line, row {row} = `origins> echo hi`, row {} = `hi`; wrong version rejected",
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
        (last.saturating_sub(2), "origins> echo end"),
        (last.saturating_sub(1), "end"),
        (last, "origins> "),
    ];
    for (row, text) in want {
        if !screen::row_matches(img, text, row) {
            return Err(format!("row {row} does not show `{text}`"));
        }
    }
    let boot = format!("ORIGINS_OS BOOT version={VERSION}");
    if screen::row_matches(img, &boot, 0) {
        return Err("BOOT line is still on row 0; the screen did not scroll".into());
    }
    Ok(format!(
        "{rows} rows; rows {}-{last} = `origins> echo end`, `end`, prompt; BOOT scrolled off",
        last - 2
    ))
}
