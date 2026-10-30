//! Builds the kernel ELF and wraps it in a UEFI-bootable GPT disk image.

use crate::config::Tools;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const KERNEL_TARGET: &str = "x86_64-unknown-none";
const KERNEL_PACKAGE: &str = "origins-os-kernel";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Panic,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Panic => "panic",
        }
    }
}

/// Each mode gets its own Cargo target dir and output dir, so a cached
/// artifact from one mode can never be packaged as the other.
pub fn build(root: &Path, tools: &Tools, mode: Mode) -> Result<PathBuf, String> {
    let target_dir = root.join("target").join(format!("kernel-{}", mode.name()));
    let mut cmd = Command::new(&tools.cargo);
    cmd.current_dir(root)
        .args([
            "build",
            "--package",
            KERNEL_PACKAGE,
            "--target",
            KERNEL_TARGET,
            "--release",
            "--locked",
        ])
        .arg("--target-dir")
        .arg(&target_dir);
    if mode == Mode::Panic {
        cmd.args(["--features", "panic-test"]);
    }
    // Host-side flags must not leak into the kernel build.
    for var in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET"] {
        cmd.env_remove(var);
    }
    println!(
        "[build] kernel ({}) -> {}",
        mode.name(),
        target_dir.display()
    );
    let status = cmd
        .status()
        .map_err(|e| format!("failed to start cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "kernel build ({}) failed with {status}",
            mode.name()
        ));
    }

    let elf = target_dir
        .join(KERNEL_TARGET)
        .join("release")
        .join(KERNEL_PACKAGE);
    if !elf.is_file() {
        return Err(format!(
            "kernel build succeeded but {} is missing",
            elf.display()
        ));
    }

    let out_dir = root.join("build").join(mode.name());
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    let img = out_dir.join(format!("origins-os-{}-uefi.img", mode.name()));
    bootloader::UefiBoot::new(&elf)
        .create_disk_image(&img)
        .map_err(|e| format!("failed to create UEFI disk image {}: {e:#}", img.display()))?;
    println!("[build] image ({}) -> {}", mode.name(), img.display());
    Ok(img)
}
