//! QEMU invocation: shared machine definition, interactive run, and the
//! bounded headless runner used by `verify`.

use crate::config::Tools;
use crate::image::Mode;
use crate::markers::{self, Expectation, Verdict};
use crate::process::{ChildGuard, Shutdown};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(50);
const KILL_BOUND: Duration = Duration::from_secs(10);

/// QEMU splits option values on `,`; a literal comma is written as `,,`.
fn opt_path(p: &Path) -> Result<String, String> {
    let s = p
        .to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", p.display()))?;
    Ok(s.replace(',', ",,"))
}

/// The machine definition shared by `run` and `verify` (see DECISIONS.md).
/// Software emulation (TCG) only, so no host hypervisor is required.
fn machine_args(tools: &Tools, image: &Path, vars: &Path) -> Result<Vec<OsString>, String> {
    let code = tools
        .ovmf_code
        .as_deref()
        .expect("resolved by resolve_for_qemu");
    let args = [
        "-machine".to_string(),
        "q35".into(),
        "-accel".into(),
        "tcg".into(),
        "-cpu".into(),
        "qemu64".into(),
        "-smp".into(),
        "1".into(),
        "-m".into(),
        "512M".into(),
        "-drive".into(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            opt_path(code)?
        ),
        "-drive".into(),
        format!("if=pflash,format=raw,unit=1,file={}", opt_path(vars)?),
        "-drive".into(),
        format!("format=raw,media=disk,file={}", opt_path(image)?),
        "-nic".into(),
        "none".into(),
        "-display".into(),
        "none".into(),
        "-monitor".into(),
        "none".into(),
        "-no-reboot".into(),
    ];
    Ok(args.into_iter().map(OsString::from).collect())
}

/// Fresh per-run directory holding a private copy of the variable store, so
/// the shared template is never written. Only generated files live here.
fn prepare_run_dir(root: &Path, case: &str, tools: &Tools) -> Result<(PathBuf, PathBuf), String> {
    let dir = root.join("build").join("runs").join(case);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("cannot clear {}: {e}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let template = tools
        .ovmf_vars_template
        .as_deref()
        .expect("resolved by resolve_for_qemu");
    let vars = dir.join("OVMF_VARS.fd");
    fs::copy(template, &vars).map_err(|e| {
        format!(
            "cannot copy variable store {} -> {}: {e}",
            template.display(),
            vars.display()
        )
    })?;
    Ok((dir, vars))
}

fn write_cmdline(dir: &Path, qemu: &Path, args: &[OsString]) {
    let mut s = format!("\"{}\"", qemu.display());
    for a in args {
        s.push_str(&format!(" \"{}\"", a.to_string_lossy()));
    }
    s.push('\n');
    let _ = fs::write(dir.join("qemu-command.txt"), s);
}

pub fn run_interactive(
    root: &Path,
    tools: &Tools,
    mode: Mode,
    image: &Path,
    timeout: Option<Duration>,
) -> Result<ExitCode, String> {
    let qemu = tools.qemu.as_deref().expect("resolved by resolve_for_qemu");
    let (dir, vars) = prepare_run_dir(root, &format!("run-{}", mode.name()), tools)?;
    let mut args = machine_args(tools, image, &vars)?;
    args.extend(["-serial", "stdio"].map(OsString::from));
    write_cmdline(&dir, qemu, &args);
    println!(
        "[run] booting {} image; guest COM1 follows. Press Ctrl+C to stop QEMU.",
        mode.name()
    );
    let child = Command::new(qemu)
        .args(&args)
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", qemu.display()))?;
    let mut guard = ChildGuard::new(child, "QEMU");
    let Some(limit) = timeout else {
        let status = guard.wait()?;
        println!("\n[run] QEMU exited with {status}");
        return Ok(ExitCode::SUCCESS);
    };
    let start = Instant::now();
    loop {
        if let Some(status) = guard.try_wait()? {
            println!("\n[run] QEMU exited with {status}");
            return Ok(ExitCode::SUCCESS);
        }
        if start.elapsed() >= limit {
            guard.shutdown(KILL_BOUND)?;
            println!("\n[run] stopped QEMU after {limit:?}");
            return Ok(ExitCode::SUCCESS);
        }
        std::thread::sleep(POLL);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    /// A wrong, out-of-order or extra marker appeared.
    UnexpectedMarker(String),
    /// The deadline passed while still waiting for a marker.
    Timeout(String),
    /// QEMU exited by itself (crash, triple fault with -no-reboot, bad args).
    EarlyExit(String),
}

impl Outcome {
    pub fn kind(&self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::UnexpectedMarker(_) => "unexpected-marker",
            Outcome::Timeout(_) => "timeout",
            Outcome::EarlyExit(_) => "early-exit",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Outcome::Pass => "all markers observed in order",
            Outcome::UnexpectedMarker(s) | Outcome::Timeout(s) | Outcome::EarlyExit(s) => s,
        }
    }
}

pub struct CaseRun {
    pub outcome: Outcome,
    pub elapsed: Duration,
    pub log: PathBuf,
    /// How QEMU was stopped; a halted guest is expected to be `Killed`.
    pub shutdown: String,
}

/// Boots `image` headlessly with COM1 written by QEMU itself to a log file,
/// and evaluates that log until pass, failure, early exit or `timeout`.
/// After a pass, keeps watching for `settle` to catch trailing markers
/// (e.g. READY after PANIC). QEMU is always terminated and reaped.
pub fn run_case(
    root: &Path,
    tools: &Tools,
    case: &str,
    image: &Path,
    exp: &Expectation,
    timeout: Duration,
    settle: Duration,
) -> Result<CaseRun, String> {
    let qemu = tools.qemu.as_deref().expect("resolved by resolve_for_qemu");
    let (dir, vars) = prepare_run_dir(root, case, tools)?;
    let log = dir.join("serial.log");
    let mut args = machine_args(tools, image, &vars)?;
    args.extend(
        [
            "-chardev".to_string(),
            format!("file,id=com1,path={}", opt_path(&log)?),
            "-serial".into(),
            "chardev:com1".into(),
        ]
        .map(OsString::from),
    );
    write_cmdline(&dir, qemu, &args);
    let open = |name: &str| {
        fs::File::create(dir.join(name)).map_err(|e| format!("cannot create {name}: {e}"))
    };
    let child = Command::new(qemu)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(open("qemu-stdout.log")?)
        .stderr(open("qemu-stderr.log")?)
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", qemu.display()))?;
    let mut guard = ChildGuard::new(child, "QEMU");

    let start = Instant::now();
    let mut passed_at: Option<Instant> = None;
    let outcome = loop {
        let text = fs::read(&log)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let verdict = markers::evaluate(exp, &text);
        match verdict {
            Verdict::Unexpected(why) => break Outcome::UnexpectedMarker(why),
            Verdict::Pass => {
                let t = *passed_at.get_or_insert_with(Instant::now);
                if t.elapsed() >= settle {
                    break Outcome::Pass;
                }
            }
            Verdict::Pending(_) => {}
        }
        if let Some(status) = guard.try_wait()? {
            // Re-read once: QEMU may have flushed final output as it exited.
            let text = fs::read(&log)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            break match markers::evaluate(exp, &text) {
                Verdict::Unexpected(why) => Outcome::UnexpectedMarker(why),
                Verdict::Pending(next) => Outcome::EarlyExit(format!(
                    "QEMU exited with {status} while waiting for `{next}`"
                )),
                Verdict::Pass => Outcome::EarlyExit(format!(
                    "QEMU exited with {status} after the markers; a halted guest should keep running"
                )),
            };
        }
        if passed_at.is_none() && start.elapsed() >= timeout {
            let next = match verdict {
                Verdict::Pending(n) => n,
                _ => "?".into(),
            };
            break Outcome::Timeout(format!("no `{next}` within {timeout:?}"));
        }
        std::thread::sleep(POLL);
    };
    let elapsed = start.elapsed();
    let shutdown = match guard.shutdown(KILL_BOUND)? {
        Shutdown::Killed => "terminated by harness".to_string(),
        Shutdown::Exited(s) => format!("exited on its own ({s})"),
        Shutdown::AlreadyReaped => "already reaped".to_string(),
    };
    Ok(CaseRun {
        outcome,
        elapsed,
        log,
        shutdown,
    })
}
