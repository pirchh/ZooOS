//! Builds the kernel ELF and wraps it in a UEFI-bootable GPT disk image.

use crate::config::Tools;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub const KERNEL_TARGET: &str = "x86_64-unknown-none";
const KERNEL_PACKAGE: &str = "zoo-os-kernel";
const USER_PACKAGE: &str = "zoo-user";
/// User programs packed into the initial filesystem as `bin/<name>`.
pub const USER_PROGRAMS: [&str; 7] = [
    "zookeeper",
    "lion-handler",
    "monkey-handler",
    "hamster-handler",
    "peacock-handler",
    "owl-handler",
    "chimp-handler",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Panic,
    FaultWriteProtect,
    FaultHeapGuard,
    FaultStackOverflow,
    TripleFault,
}

impl Mode {
    pub const ALL: [Mode; 6] = [
        Mode::Normal,
        Mode::Panic,
        Mode::FaultWriteProtect,
        Mode::FaultHeapGuard,
        Mode::FaultStackOverflow,
        Mode::TripleFault,
    ];

    /// Name used on the command line and for build/output directories.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Normal => "normal",
            Mode::Panic => "panic",
            Mode::FaultWriteProtect => "fault-write-protect",
            Mode::FaultHeapGuard => "fault-heap-guard",
            Mode::FaultStackOverflow => "fault-stack-overflow",
            Mode::TripleFault => "triple-fault",
        }
    }

    /// Kernel Cargo feature selecting this mode (None for the normal kernel).
    fn feature(self) -> Option<&'static str> {
        match self {
            Mode::Normal => None,
            Mode::Panic => Some("panic-test"),
            other => Some(other.name()),
        }
    }

    /// What the kernel prints in its `INFO mode=` line.
    pub fn kernel_mode(self) -> &'static str {
        self.feature().unwrap_or("normal")
    }

    pub fn parse(s: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == s)
    }
}

/// Builds the user programs (static, non-relocatable ELF at 0x200000; see
/// docs/m3-design.md §7) and packs them into `build/initrd.tar`. Done once
/// per xtask run; every image gets the same archive.
fn initrd(root: &Path, tools: &Tools) -> Result<PathBuf, String> {
    static DONE: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DONE.get_or_init(|| build_initrd(root, tools)).clone()
}

fn build_initrd(root: &Path, tools: &Tools) -> Result<PathBuf, String> {
    let target_dir = root.join("target").join("user");
    let mut cmd = Command::new(&tools.cargo);
    cmd.current_dir(root)
        .args([
            "build",
            "--package",
            USER_PACKAGE,
            "--target",
            KERNEL_TARGET,
            "--release",
            "--locked",
        ])
        .arg("--target-dir")
        .arg(&target_dir);
    for var in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET"] {
        cmd.env_remove(var);
    }
    // Only user programs are built with this; the kernel keeps its own flags.
    cmd.env(
        "CARGO_TARGET_X86_64_UNKNOWN_NONE_RUSTFLAGS",
        "-C relocation-model=static",
    );
    println!("[build] user programs -> {}", target_dir.display());
    let status = cmd
        .status()
        .map_err(|e| format!("failed to start cargo: {e}"))?;
    if !status.success() {
        return Err(format!("user program build failed with {status}"));
    }
    let mut archive = Vec::new();
    for name in USER_PROGRAMS {
        let path = target_dir.join(KERNEL_TARGET).join("release").join(name);
        let data =
            std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let entry = format!("bin/{name}");
        let header = zoo_abi::tar::header(&entry, data.len())
            .ok_or_else(|| format!("cannot make a tar header for {entry}"))?;
        archive.extend_from_slice(&header);
        archive.extend_from_slice(&data);
        archive.resize(
            archive.len().div_ceil(zoo_abi::tar::BLOCK) * zoo_abi::tar::BLOCK,
            0,
        );
    }
    archive.extend_from_slice(&[0; 2 * zoo_abi::tar::BLOCK]);
    // The kernel's reader must accept exactly what we wrote.
    let count = zoo_abi::tar::read(&archive)
        .map(|e| e.map_err(|e| format!("initrd self-check failed: {e:?}")))
        .collect::<Result<Vec<_>, _>>()?
        .len();
    if count != USER_PROGRAMS.len() {
        return Err(format!("initrd self-check found {count} files"));
    }
    let out_dir = root.join("build");
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    let out = out_dir.join("initrd.tar");
    std::fs::write(&out, &archive).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    println!(
        "[build] initrd ({count} programs, {} bytes) -> {}",
        archive.len(),
        out.display()
    );
    Ok(out)
}

/// Each mode gets its own Cargo target dir and output dir, so a cached
/// artifact from one mode can never be packaged as the other.
pub fn build(root: &Path, tools: &Tools, mode: Mode) -> Result<PathBuf, String> {
    let initrd = initrd(root, tools)?;
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
    if let Some(feature) = mode.feature() {
        cmd.args(["--features", feature]);
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
    let img = out_dir.join(format!("zoo-os-{}-uefi.img", mode.name()));
    bootloader::UefiBoot::new(&elf)
        .set_ramdisk(&initrd)
        .create_disk_image(&img)
        .map_err(|e| format!("failed to create UEFI disk image {}: {e:#}", img.display()))?;
    println!("[build] image ({}) -> {}", mode.name(), img.display());
    Ok(img)
}
