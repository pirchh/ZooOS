//! Host build utility for ZooOS. Run via `cargo xtask <command>`.

mod config;
mod cprog;
mod doctor;
mod font;
mod image;
mod infer;
mod markers;
mod musl;
mod process;
mod qemu;
mod qmp;
mod requant;
mod screen;
mod verify;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "\
usage: cargo xtask <command> [options]

commands:
  doctor                    check prerequisites (installs nothing)
  build [--mode M]          build kernel(s) and UEFI disk image(s)
                            M = all (default) | normal | fast-boot | big-screen | panic
                                | fault-write-protect | fault-heap-guard
                                | fault-stack-overflow | triple-fault
  usb-image                 build the everyday (fast-boot) image as
                            build/usb/zoo-os-usb.img for a USB stick; writes
                            no device (docs/usb-boot.md)
  run [--mode M | --panic | --model] [--accel tcg|whpx|kvm] [--timeout-secs N]
                            build one mode (default fast-boot: no boot-check
                            programs), then boot it in QEMU
                            with COM1 on this terminal
                            (Ctrl+C stops QEMU; no timeout unless given).
                            --model: the normal image with the assistant's
                            model (tools/models/); use with --accel
  requant --bella NAME     write a Q4_0 copy of a Q8_0 model (docs/qwen3-design.md)
  embed-check              the embedding model against its model card (docs/vectors-design.md)
  infer [--prompt TEXT] [--tokens N]
                            run the model on the host (tools/models/), print
                            prompt/output token ids, top logits and timing
  infer-check [--tokens N]  compare the engine with the NumPy reference
  assist-check [--prompt S]  guided tool choice on fixed requests (host)
  (run, infer, infer-check, assist-check, verify: --bella 0.5b|1.5b picks
   Bella's model in tools/models/; 0.5b is the default. infer, infer-check,
   assist-check: --portable runs the host engine without AVX2)
                            (tools/reference/) on several prompts
  verify [--timeout-secs N] [--negative-timeout-secs N] [--settle-ms N]
         [--accel tcg|whpx|kvm] [--skip-model] [--case NAME]
                            build every mode, then run all positive and negative
                            cases headlessly (defaults: 30, 5, 1000). The
                            inference case needs tools/models/; --skip-model
                            reports it as not run instead of failing.

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
    if let Some(name) = &opts.bella {
        infer::select_model(name)?;
    }
    if opts.portable {
        zoo_infer::cpu::force_portable();
    }
    if opts.f32_activations {
        zoo_infer::quant::set_int8(false);
    }
    let root = workspace_root();
    match cmd.as_str() {
        "doctor" => {
            opts.reject_all()?;
            Ok(doctor::run(&root))
        }
        "usb-image" => {
            // Builds a file only; writing it to a USB stick is the owner's
            // step, on a device they choose (docs/usb-boot.md).
            opts.reject_all()?;
            let tools = config::Tools::resolve_build_only();
            // The model too, when it is there (docs/a3-design.md).
            let img = match infer::load_model(&root) {
                Ok(data) if !opts.no_model => {
                    let r = infer::run(&data, infer::GUEST_PROMPT, infer::GUEST_TOKENS)?;
                    println!("[usb-image] with Bella's model (--no-model to leave it out)");
                    image::build_model_image(
                        &root,
                        &tools,
                        image::Mode::FastBoot,
                        &data,
                        infer::GUEST_PROMPT,
                        &r.output_ids,
                    )?
                }
                _ => image::build(&root, &tools, image::Mode::FastBoot)?,
            };
            let dir = root.join("build").join("usb");
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            let out = dir.join("zoo-os-usb.img");
            std::fs::copy(&img, &out)
                .map_err(|e| format!("cannot copy to {}: {e}", out.display()))?;
            let bytes =
                std::fs::read(&out).map_err(|e| format!("cannot read {}: {e}", out.display()))?;
            use sha2::{Digest, Sha256};
            let hex: String = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            println!("[usb-image] {} ({} bytes)", out.display(), bytes.len());
            println!("[usb-image] sha256 {hex}");
            println!(
                "[usb-image] Write it to a USB stick you choose (Rufus in DD mode, or balenaEtcher),\n\
                 [usb-image] checking the target is the stick. Boot it only on another PC, never this one.\n\
                 [usb-image] See docs/usb-boot.md."
            );
            Ok(ExitCode::SUCCESS)
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
            let mode = match (opts.panic, opts.modes()?.as_slice()) {
                (true, _) if opts.mode.is_some() => {
                    return Err("use either --panic or --mode, not both".into());
                }
                (true, _) => image::Mode::Panic,
                (false, _) if opts.mode.is_none() => image::Mode::FastBoot,
                (false, [one]) => *one,
                _ => return Err("run takes exactly one mode, not `all`".into()),
            };
            // `--accel` was accepted here but never applied, so every
            // session ran under TCG (docs/verification.md, run fix).
            if let Some(a) = &opts.accel {
                qemu::set_accel(a);
            }
            // The everyday system uses the processors (docs/smp-design.md):
            // 8 unless `--cpus` says otherwise.
            qemu::set_cpus(opts.cpus.unwrap_or(8));
            let tools = config::Tools::resolve_for_qemu()?;
            if opts.model {
                // The normal image with Bella's model added (as the
                // `bella-live` case uses), about 410 MB larger.
                if opts.mode.is_some() || opts.panic {
                    return Err("--model uses the normal image; drop --mode/--panic".into());
                }
                let data = infer::load_model(&root)?;
                let r = infer::run(&data, infer::GUEST_PROMPT, infer::GUEST_TOKENS)?;
                let img = image::build_model_image(
                    &root,
                    &tools,
                    image::Mode::Normal,
                    &data,
                    infer::GUEST_PROMPT,
                    &r.output_ids,
                )?;
                return qemu::run_interactive(
                    &root,
                    &tools,
                    image::Mode::Normal,
                    &img,
                    opts.timeout,
                );
            }
            // The everyday system gets Bella's model on the read-only
            // model disk when it is in tools/models/ (docs/stage2-design.md
            // §3): the boot image stays small; `--no-model` leaves it out.
            if mode == image::Mode::FastBoot && !opts.no_model {
                match image::model_disk(&root) {
                    Ok(disk) => {
                        println!(
                            "[run] Bella's model is on the model disk {} (--no-model to leave it out)",
                            disk.display()
                        );
                        qemu::set_model_disk(Some(disk));
                        // The 1.5B model needs more than 2 GiB with the
                        // desktop beside it.
                        if infer::model_file() != infer::MODELS[0].1 {
                            qemu::set_memory_gib(4);
                        }
                    }
                    Err(e) => {
                        println!("[run] no model ({e}); Bella will answer typed commands only")
                    }
                }
            }
            let img = image::build(&root, &tools, mode)?;
            qemu::run_interactive(&root, &tools, mode, &img, opts.timeout)
        }
        "verify" => {
            let tools = config::Tools::resolve_for_qemu()?;
            if let Some(a) = &opts.accel {
                qemu::set_accel(a);
            }
            let settings = verify::Settings {
                skip_model: opts.skip_model,
                only: opts.case.clone(),
                timeout: opts.timeout.unwrap_or(Duration::from_secs(30)),
                negative_timeout: opts.negative_timeout.unwrap_or(Duration::from_secs(5)),
                settle: opts.settle.unwrap_or(Duration::from_millis(1000)),
                cpus: opts.cpus.unwrap_or(1),
            };
            verify::run(&root, &tools, &settings)
        }
        "embed-check" => {
            // The embedding model against its model card (docs/vectors-design.md).
            if infer::embed_check(&root)? {
                println!("embed-check: PASS");
                Ok(ExitCode::SUCCESS)
            } else {
                println!("embed-check: FAIL");
                Ok(ExitCode::from(1))
            }
        }
        "requant" => {
            // Q8_0 -> Q4_0 for the chosen model (docs/qwen3-design.md).
            let input = infer::model_path(&root);
            let name = input
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or("bad model file name")?
                .replace("q8_0", "q4_0");
            if name == input.file_name().and_then(|n| n.to_str()).unwrap_or("") {
                return Err(format!("{} is not a q8_0 file", input.display()));
            }
            requant::run(&input, &input.with_file_name(name))?;
            Ok(ExitCode::SUCCESS)
        }
        "infer" => {
            let data = infer::load_model(&root)?;
            let prompt = opts
                .prompt
                .clone()
                .unwrap_or_else(|| "What is the capital of France?".into());
            let system = match &opts.system_file {
                Some(f) => {
                    std::fs::read_to_string(f).map_err(|e| format!("cannot read {f}: {e}"))?
                }
                None => infer::SYSTEM.to_string(),
            };
            let r = infer::run_with_system(&data, &system, &prompt, opts.tokens.unwrap_or(16))?;
            println!("prompt_ids={}", infer::ids(&r.prompt_ids));
            println!("output_ids={}", infer::ids(&r.output_ids));
            print!("first_top=");
            for (i, (id, l)) in r.first_top.iter().enumerate() {
                print!("{}{id}:{l:.4}", if i == 0 { "" } else { "," });
            }
            println!();
            println!("text={:?}", r.text);
            println!(
                "prefill {} tokens in {:.2}s ({:.2} tok/s); decoded {} tokens in {:.2}s ({:.2} tok/s)",
                r.prompt_ids.len(),
                r.prefill_secs,
                r.prompt_ids.len() as f64 / r.prefill_secs,
                r.output_ids.len(),
                r.decode_secs,
                r.output_ids.len() as f64 / r.decode_secs.max(1e-9)
            );
            Ok(ExitCode::SUCCESS)
        }
        "model-image" => {
            let tools = config::Tools::resolve_build_only();
            let data = infer::load_model(&root)?;
            let r = infer::run(&data, infer::GUEST_PROMPT, infer::GUEST_TOKENS)?;
            image::build_model_image(
                &root,
                &tools,
                image::Mode::Normal,
                &data,
                infer::GUEST_PROMPT,
                &r.output_ids,
            )?;
            println!("expected output ids: {}", infer::ids(&r.output_ids));
            Ok(ExitCode::SUCCESS)
        }
        "assist-check" => {
            let ok = infer::assist_check(&root, opts.prompt.as_deref())?;
            println!("assist-check: {}", if ok { "PASS" } else { "FAIL" });
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        "infer-check" => {
            let ok = infer::check(&root, opts.tokens.unwrap_or(24))?;
            println!("infer-check: {}", if ok { "PASS" } else { "FAIL" });
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
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
    prompt: Option<String>,
    tokens: Option<usize>,
    accel: Option<String>,
    skip_model: bool,
    model: bool,
    /// `run` and `usb-image`: leave Bella's model out.
    no_model: bool,
    case: Option<String>,
    system_file: Option<String>,
    /// `--bella <0.5b|1.5b>`: Bella's model (infer::MODELS).
    bella: Option<String>,
    /// `--portable`: the host engine without AVX2 (docs/s3-design.md).
    portable: bool,
    /// `--f32-activations`: no 8-bit activations (docs/int8-design.md).
    f32_activations: bool,
    /// `--cpus N`: the guest's processors (docs/smp-design.md).
    cpus: Option<u8>,
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
                "--skip-model" => o.skip_model = true,
                "--model" => o.model = true,
                "--no-model" => o.no_model = true,
                "--case" => o.case = Some(value("--case")?),
                "--bella" => o.bella = Some(value("--bella")?),
                "--portable" => o.portable = true,
                "--f32-activations" => o.f32_activations = true,
                "--cpus" => {
                    let v = value("--cpus")?;
                    o.cpus = Some(
                        v.parse()
                            .ok()
                            .filter(|n| (1..=32).contains(n))
                            .ok_or_else(|| format!("invalid --cpus `{v}` (1 to 32)"))?,
                    );
                }
                "--system-file" => o.system_file = Some(value("--system-file")?),
                "--accel" => {
                    let v = value("--accel")?;
                    if v != "tcg" && v != "whpx" && v != "kvm" {
                        return Err(format!("invalid --accel `{v}` (tcg, whpx or kvm)"));
                    }
                    o.accel = Some(v);
                }
                "--prompt" => o.prompt = Some(value("--prompt")?),
                "--tokens" => {
                    let v = value("--tokens")?;
                    o.tokens = Some(v.parse().map_err(|_| format!("invalid --tokens `{v}`"))?);
                }
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
            None | Some("all") => Ok(image::Mode::ALL.to_vec()),
            Some(m) => image::Mode::parse(m).map(|m| vec![m]).ok_or_else(|| {
                let names: Vec<&str> = image::Mode::ALL.iter().map(|m| m.name()).collect();
                format!("invalid --mode `{m}` (expected all, {})", names.join(", "))
            }),
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
