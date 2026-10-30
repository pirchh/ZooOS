//! `cargo xtask doctor`: reports prerequisite status. Installs nothing.

use crate::config::{self, Found};
use crate::image::KERNEL_TARGET;
use crate::process::output_bounded;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::Duration;

const CMD_TIMEOUT: Duration = Duration::from_secs(20);

struct Report {
    failures: usize,
}

impl Report {
    fn ok(&self, what: &str, detail: &str) {
        println!("  [ ok ] {what}: {detail}");
    }
    fn fail(&mut self, what: &str, detail: &str, fix: &str) {
        self.failures += 1;
        println!("  [FAIL] {what}: {detail}\n         fix: {fix}");
    }
}

pub fn run(root: &Path) -> ExitCode {
    let mut r = Report { failures: 0 };
    println!("OriginsOS doctor (workspace: {})", root.display());

    check_toolchain(root, &mut r);

    if root.join("Cargo.lock").is_file() {
        r.ok("Cargo.lock", "present (builds use --locked)");
    } else {
        r.fail(
            "Cargo.lock",
            "missing",
            "restore Cargo.lock from version control",
        );
    }

    let qemu = config::find_qemu();
    check_qemu(&qemu, &mut r);
    let code = config::find_ovmf_code(qemu.path.as_deref());
    check_firmware("OVMF_CODE", &code, &mut r);
    let vars = config::find_ovmf_vars(qemu.path.as_deref());
    check_firmware("OVMF_VARS_TEMPLATE", &vars, &mut r);

    if r.failures == 0 {
        println!("ready: all prerequisites found");
        ExitCode::SUCCESS
    } else {
        println!(
            "not ready: {} prerequisite(s) missing; see tools/firmware/README.md",
            r.failures
        );
        ExitCode::FAILURE
    }
}

fn pinned_channel(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("rust-toolchain.toml")).ok()?;
    text.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("channel"))
        .and_then(|rest| rest.split('"').nth(1))
        .map(str::to_string)
}

fn check_toolchain(root: &Path, r: &mut Report) {
    let Some(channel) = pinned_channel(root) else {
        r.fail(
            "rust-toolchain.toml",
            "missing or has no channel",
            "restore rust-toolchain.toml",
        );
        return;
    };
    // rustup sets RUSTUP_TOOLCHAIN for processes it launches via `cargo`.
    match std::env::var("RUSTUP_TOOLCHAIN") {
        Ok(active) if active.starts_with(&channel) => r.ok("toolchain", &format!("{active} (pinned {channel})")),
        Ok(active) => r.fail(
            "toolchain",
            &format!("active toolchain is {active}, pinned is {channel}"),
            "remove the override (RUSTUP_TOOLCHAIN / `rustup override`) so rust-toolchain.toml applies",
        ),
        Err(_) => r.fail(
            "toolchain",
            "cannot confirm the active toolchain (rustup not in use?)",
            &format!("install rustup; it will install {channel} from rust-toolchain.toml"),
        ),
    }

    let mut cmd = Command::new("rustc");
    cmd.current_dir(root).arg("-vV");
    match output_bounded(cmd, CMD_TIMEOUT, "rustc") {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let get = |k: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(k))
                    .unwrap_or("?")
                    .trim()
                    .to_string()
            };
            r.ok(
                "rustc",
                &format!(
                    "{} ({}, host {})",
                    get("release:"),
                    get("commit-date:"),
                    get("host:")
                ),
            );
        }
        Ok(o) => r.fail(
            "rustc",
            &format!("`rustc -vV` failed: {}", o.status),
            "check the Rust installation",
        ),
        Err(e) => r.fail("rustc", &e, "install Rust via rustup"),
    }

    let mut cmd = Command::new("rustc");
    cmd.current_dir(root).args(["--print", "sysroot"]);
    let sysroot = match output_bounded(cmd, CMD_TIMEOUT, "rustc") {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => {
            return r.fail(
                "sysroot",
                "cannot query rustc sysroot",
                "check the Rust installation",
            );
        }
    };
    let rustlib = Path::new(&sysroot).join("lib").join("rustlib");
    let fix_component = format!("rustup component add rust-src --toolchain {channel}");
    if rustlib.join("src").join("rust").join("library").is_dir() {
        r.ok(
            "rust-src",
            "installed (needed by the bootloader's -Zbuild-std)",
        );
    } else {
        r.fail("rust-src", "component not installed", &fix_component);
    }
    if rustlib.join(KERNEL_TARGET).is_dir() {
        r.ok("kernel target", &format!("{KERNEL_TARGET} installed"));
    } else {
        r.fail(
            "kernel target",
            &format!("{KERNEL_TARGET} not installed"),
            &format!("rustup target add {KERNEL_TARGET} --toolchain {channel}"),
        );
    }
}

fn check_qemu(found: &Found, r: &mut Report) {
    let Some(path) = &found.path else {
        return r.fail(
            "QEMU",
            &found.source,
            "install QEMU (Windows: `winget install SoftwareFreedomConservancy.QEMU`) or set QEMU_X86_64",
        );
    };
    let mut cmd = Command::new(path);
    cmd.arg("--version");
    match output_bounded(cmd, CMD_TIMEOUT, "qemu-system-x86_64 --version") {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout);
            let first = v.lines().next().unwrap_or("?").trim();
            r.ok(
                "QEMU",
                &format!("{first} at {} ({})", path.display(), found.source),
            );
        }
        Ok(o) => r.fail(
            "QEMU",
            &format!("{} --version failed: {}", path.display(), o.status),
            "reinstall QEMU",
        ),
        Err(e) => r.fail("QEMU", &e, "reinstall QEMU or set QEMU_X86_64"),
    }
}

fn check_firmware(var: &str, found: &Found, r: &mut Report) {
    let Some(path) = &found.path else {
        return r.fail(
            var,
            &found.source,
            &format!("set {var}; see tools/firmware/README.md"),
        );
    };
    match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => {
            let hex: String = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            r.ok(
                var,
                &format!(
                    "{} ({} bytes, sha256 {hex}; {})",
                    path.display(),
                    bytes.len(),
                    found.source
                ),
            );
        }
        Ok(_) => r.fail(
            var,
            &format!("{} is empty", path.display()),
            "reinstall the firmware",
        ),
        Err(e) => r.fail(
            var,
            &format!("cannot read {}: {e}", path.display()),
            "check permissions",
        ),
    }
}
