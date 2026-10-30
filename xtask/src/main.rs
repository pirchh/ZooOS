//! Host build utility for OriginsOS. Run via `cargo xtask <command>`.

mod config;
mod doctor;
mod image;
mod markers;
mod process;
mod qemu;
mod verify;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "\
usage: cargo xtask <command> [options]

commands:
  doctor                    check prerequisites (installs nothing)
  build [--mode M]          build kernel(s) and UEFI disk image(s)
                            M = normal | panic | all (default: all)
  run [--panic] [--timeout-secs N]
                            build, then boot in QEMU with COM1 on this terminal
                            (Ctrl+C stops QEMU; no timeout unless given)
  verify [--timeout-secs N] [--negative-timeout-secs N] [--settle-ms N]
                            build both images, then run the normal, panic and
                            negative cases headlessly (defaults: 30, 5, 1000)

configuration (environment):
  QEMU_X86_64               path to qemu-system-x86_64
  OVMF_CODE                 path to the OVMF/EDK II x86-64 code image
  OVMF_VARS_TEMPLATE        path to the OVMF variable-store template (never modified)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<ExitCode, String> {
    let Some((cmd, rest)) = args.split_first() else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let opts = Options::parse(rest)?;
    let root = workspace_root();
    match cmd.as_str() {
        "doctor" => {
            opts.reject_all()?;
            Ok(doctor::run(&root))
        }
        "build" => {
            let modes = opts.modes()?;
            let tools = config::Tools::resolve_build_only();
            for mode in modes {
                image::build(&root, &tools, mode)?;
            }
            Ok(ExitCode::SUCCESS)
        }
        "run" => {
            let mode = if opts.panic {
                image::Mode::Panic
            } else {
                image::Mode::Normal
            };
            let tools = config::Tools::resolve_for_qemu()?;
            let img = image::build(&root, &tools, mode)?;
            qemu::run_interactive(&root, &tools, mode, &img, opts.timeout)
        }
        "verify" => {
            let tools = config::Tools::resolve_for_qemu()?;
            let settings = verify::Settings {
                timeout: opts.timeout.unwrap_or(Duration::from_secs(30)),
                negative_timeout: opts.negative_timeout.unwrap_or(Duration::from_secs(5)),
                settle: opts.settle.unwrap_or(Duration::from_millis(1000)),
            };
            verify::run(&root, &tools, &settings)
        }
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    }
}

fn workspace_root() -> PathBuf {
    // xtask/ lives directly under the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest dir has a parent")
        .to_path_buf()
}

#[derive(Default)]
struct Options {
    mode: Option<String>,
    panic: bool,
    timeout: Option<Duration>,
    negative_timeout: Option<Duration>,
    settle: Option<Duration>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut o = Options::default();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            let mut value = |name: &str| {
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match a.as_str() {
                "--mode" => o.mode = Some(value("--mode")?),
                "--panic" => o.panic = true,
                "--timeout-secs" => o.timeout = Some(secs(&value("--timeout-secs")?)?),
                "--negative-timeout-secs" => {
                    o.negative_timeout = Some(secs(&value("--negative-timeout-secs")?)?)
                }
                "--settle-ms" => {
                    let v = value("--settle-ms")?;
                    let ms: u64 = v
                        .parse()
                        .map_err(|_| format!("invalid --settle-ms `{v}`"))?;
                    o.settle = Some(Duration::from_millis(ms));
                }
                other => return Err(format!("unknown option `{other}`\n\n{USAGE}")),
            }
        }
        Ok(o)
    }

    fn reject_all(&self) -> Result<(), String> {
        if self.mode.is_some() || self.panic || self.timeout.is_some() {
            return Err("doctor takes no options".into());
        }
        Ok(())
    }

    fn modes(&self) -> Result<Vec<image::Mode>, String> {
        match self.mode.as_deref() {
            None | Some("all") => Ok(vec![image::Mode::Normal, image::Mode::Panic]),
            Some("normal") => Ok(vec![image::Mode::Normal]),
            Some("panic") => Ok(vec![image::Mode::Panic]),
            Some(m) => Err(format!(
                "invalid --mode `{m}` (expected normal, panic or all)"
            )),
        }
    }
}

fn secs(v: &str) -> Result<Duration, String> {
    match v.parse::<u64>() {
        Ok(n) if n > 0 => Ok(Duration::from_secs(n)),
        _ => Err(format!(
            "invalid number of seconds `{v}` (must be a positive integer)"
        )),
    }
}
