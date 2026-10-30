//! `cargo xtask verify`: positive and negative QEMU boot cases.

use crate::config::Tools;
use crate::image::{self, Mode};
use crate::markers::{Expectation, Matcher};
use crate::qemu::{self, Outcome};
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

struct Case {
    name: &'static str,
    mode: Mode,
    expectation: Expectation,
    timeout: Duration,
    /// The outcome kind this case must produce for verification to succeed.
    want: &'static str,
    purpose: &'static str,
}

pub fn run(root: &Path, tools: &Tools, s: &Settings) -> Result<ExitCode, String> {
    let normal_img = image::build(root, tools, Mode::Normal)?;
    let panic_img = image::build(root, tools, Mode::Panic)?;

    let mut never = Expectation::normal(VERSION);
    never
        .required
        .push(Matcher::Exact("ORIGINS_OS NEVER-EMITTED".into()));

    let cases = [
        Case {
            name: "normal",
            mode: Mode::Normal,
            expectation: Expectation::normal(VERSION),
            timeout: s.timeout,
            want: "pass",
            purpose: "normal image emits BOOT, INFO, READY in order and no PANIC",
        },
        Case {
            name: "panic",
            mode: Mode::Panic,
            expectation: Expectation::panic(VERSION),
            timeout: s.timeout,
            want: "pass",
            purpose: "panic image emits BOOT, INFO, PANIC in order and no READY",
        },
        Case {
            name: "negative-wrong-marker",
            mode: Mode::Panic,
            expectation: Expectation::normal(VERSION),
            timeout: s.timeout,
            want: "unexpected-marker",
            purpose: "verifier must reject the panic image under normal expectations",
        },
        Case {
            name: "negative-timeout",
            mode: Mode::Normal,
            expectation: never,
            timeout: s.negative_timeout,
            want: "timeout",
            purpose: "verifier must reject a marker that never appears, by timeout",
        },
    ];

    let mut report = String::new();
    let mut all_ok = true;
    for c in &cases {
        let img = match c.mode {
            Mode::Normal => &normal_img,
            Mode::Panic => &panic_img,
        };
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
            c.timeout,
            s.settle,
        )?;
        let ok = r.outcome.kind() == c.want;
        all_ok &= ok;
        let line = format!(
            "{:<5} {:<22} expected={:<17} got={:<17} {:>6.1}s  qemu {}\n      detail: {}\n      log: {}\n",
            if ok { "OK" } else { "FAIL" },
            c.name,
            c.want,
            r.outcome.kind(),
            r.elapsed.as_secs_f64(),
            r.shutdown,
            r.outcome.detail(),
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
            "[verify] PASS: 2 positive cases passed, 2 negative cases were rejected as intended"
        );
        Ok(ExitCode::SUCCESS)
    } else {
        println!("[verify] FAIL");
        Ok(ExitCode::FAILURE)
    }
}
