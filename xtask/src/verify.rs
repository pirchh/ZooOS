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

    let normal = expect(Mode::Normal, &with_tests("ORIGINS_OS READY"));
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
        },
        Case {
            name: "negative-wrong-marker",
            mode: Mode::Panic,
            expectation: normal.clone(),
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
        Case {
            name: "negative-triple-fault",
            mode: Mode::TripleFault,
            expectation: expect(Mode::TripleFault, &["ORIGINS_OS READY"]),
            timeout: s.timeout,
            want: "early-exit",
            purpose: "verifier must reject a guest that triple-faults (reset with -no-reboot)",
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
            "[verify] PASS: all positive cases passed and all negative cases were rejected as intended"
        );
        Ok(ExitCode::SUCCESS)
    } else {
        println!("[verify] FAIL");
        Ok(ExitCode::FAILURE)
    }
}
