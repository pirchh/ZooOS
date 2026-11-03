//! Builds the kernel ELF and wraps it in a UEFI-bootable GPT disk image.

use crate::config::Tools;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub const KERNEL_TARGET: &str = "x86_64-unknown-none";
const KERNEL_PACKAGE: &str = "zoo-os-kernel";
const USER_PACKAGE: &str = "zoo-user";
/// Custom user-program target (`user/x86_64-zoo.json`).
const USER_TARGET: &str = "x86_64-zoo";
/// User programs packed into the initial filesystem as `bin/<name>`.
pub const USER_PROGRAMS: [&str; 25] = [
    "zookeeper",
    "beluga-test",
    "lemming-test",
    "muskox-test",
    "peacock-handler",
    "files",
    "crash-test",
    "narwhal-test",
    "elephant-handler",
    "otter-handler",
    "bella",
    "caribou-test",
    "walrus-test",
    "wolverine-test",
    "arcticfox-test",
    "beaver-handler",
    "seal-test",
    "ptarmigan-test",
    "badger-handler",
    "notes",
    "bowhead-test",
    "snowgoose-test",
    "gyrfalcon-test",
    "hare-test",
    "network",
];

/// Sample documents packed as `home/user/...` (docs/m5c1-design.md §1).
const SEED_DIR: &str = "seed/home/user";
/// System configuration packed as `etc/...` (docs/s1-design.md).
const ETC_DIR: &str = "seed/etc";

/// Files under `dir`, as paths relative to it with `/` separators, sorted.
fn seed_files(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut out = Vec::new();
    let mut stack = vec![(String::new(), dir.to_path_buf())];
    while let Some((rel, path)) = stack.pop() {
        let entries =
            std::fs::read_dir(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|n| format!("seed file name is not UTF-8: {n:?}"))?;
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let kind = entry
                .file_type()
                .map_err(|e| format!("cannot stat {}: {e}", entry.path().display()))?;
            if kind.is_dir() {
                stack.push((child, entry.path()));
            } else if kind.is_file() {
                out.push((child, entry.path()));
            }
        }
    }
    out.sort();
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// The normal system without the boot-check programs
    /// (docs/b1-design.md): what `cargo xtask run` boots by default.
    FastBoot,
    /// The normal system on a screen wider than 1920 (test mode).
    BigScreen,
    Panic,
    FaultWriteProtect,
    FaultHeapGuard,
    FaultStackOverflow,
    TripleFault,
}

impl Mode {
    pub const ALL: [Mode; 8] = [
        Mode::Normal,
        Mode::FastBoot,
        Mode::BigScreen,
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
            Mode::FastBoot => "fast-boot",
            Mode::BigScreen => "big-screen",
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
        .args(["build", "--package", USER_PACKAGE, "--release", "--locked"])
        // User programs use SSE (docs/m5a-design.md §1): a custom target
        // with core/alloc rebuilt for it. Static, non-relocatable output.
        .arg("--target")
        .arg(root.join("user").join(format!("{USER_TARGET}.json")))
        .args([
            "-Zjson-target-spec",
            "-Zbuild-std=core,alloc,compiler_builtins",
            "-Zbuild-std-features=compiler-builtins-mem",
        ])
        .arg("--target-dir")
        .arg(&target_dir);
    for var in ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET"] {
        cmd.env_remove(var);
    }
    println!("[build] user programs -> {}", target_dir.display());
    let status = cmd
        .status()
        .map_err(|e| format!("failed to start cargo: {e}"))?;
    if !status.success() {
        return Err(format!("user program build failed with {status}"));
    }
    let mut archive = Vec::new();
    for name in USER_PROGRAMS {
        let path = target_dir.join(USER_TARGET).join("release").join(name);
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
    // C programs, when the pinned toolchain is present (docs/posix5-design.md).
    let c_programs = match crate::cprog::zig(root) {
        Some(zig) => crate::cprog::build(root, &zig)?,
        None => {
            println!(
                "[build] no C toolchain in tools/zig/ (see tools/zig/README.md); C programs skipped"
            );
            Vec::new()
        }
    };
    // The UI font atlas (docs/u1-design.md).
    let fonts = crate::font::build_atlas(root)?;
    {
        let entry = "ui/fonts.zfnt";
        let header = zoo_abi::tar::header(entry, fonts.len())
            .ok_or_else(|| format!("cannot make a tar header for {entry}"))?;
        archive.extend_from_slice(&header);
        archive.extend_from_slice(&fonts);
        archive.resize(
            archive.len().div_ceil(zoo_abi::tar::BLOCK) * zoo_abi::tar::BLOCK,
            0,
        );
    }
    for (entry, data) in &c_programs {
        let header = zoo_abi::tar::header(entry, data.len())
            .ok_or_else(|| format!("cannot make a tar header for {entry}"))?;
        archive.extend_from_slice(&header);
        archive.extend_from_slice(data);
        archive.resize(
            archive.len().div_ceil(zoo_abi::tar::BLOCK) * zoo_abi::tar::BLOCK,
            0,
        );
    }
    // Sample home files, then system configuration (`seed/etc` -> `etc/`).
    let mut seeds: Vec<(String, PathBuf)> = seed_files(&root.join(SEED_DIR))?
        .into_iter()
        .map(|(rel, p)| (format!("home/user/{rel}"), p))
        .collect();
    seeds.extend(
        seed_files(&root.join(ETC_DIR))?
            .into_iter()
            .map(|(rel, p)| (format!("etc/{rel}"), p)),
    );
    for (entry, path) in &seeds {
        let data =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let header = zoo_abi::tar::header(entry, data.len())
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
    if count != USER_PROGRAMS.len() + c_programs.len() + seeds.len() + 1 {
        return Err(format!("initrd self-check found {count} files"));
    }
    let out_dir = root.join("build");
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    let out = out_dir.join("initrd.tar");
    std::fs::write(&out, &archive).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    println!(
        "[build] initrd ({} programs, {} in C, {} home files, {} bytes) -> {}",
        USER_PROGRAMS.len() + c_programs.len(),
        c_programs.len(),
        seeds.len(),
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
    let mut boot = bootloader::UefiBoot::new(&elf);
    boot.set_ramdisk(&initrd);
    let mut config = bootloader::BootConfig::default();
    if matches!(mode, Mode::BigScreen) {
        // Ask the firmware for a screen wider than the desktop's back
        // buffer, as on large monitors (docs/usb-boot.md).
        config.frame_buffer.minimum_framebuffer_width = Some(2000);
    }
    boot.set_boot_config(&config);
    boot.create_disk_image(&img)
        .map_err(|e| format!("failed to create UEFI disk image {}: {e:#}", img.display()))?;
    println!("[build] image ({}) -> {}", mode.name(), img.display());
    Ok(img)
}

/// The Wine image (docs/linux10-design.md): the fast-boot kernel with the
/// normal initrd plus the exported Wine build (`build/wine/`, made by
/// tools/wine/export.sh). Wine's programs name musl's libc as
/// `libc.musl-x86_64.so.1` (Alpine's name); musl's loader treats any
/// `libc.` name as itself, so no file of that name is needed. `None`
/// without the export.
pub fn build_wine_image(root: &Path, tools: &Tools) -> Result<Option<PathBuf>, String> {
    let export = root.join("build").join("wine").join("wine-11.0-zooos.tar");
    if !export.is_file() {
        return Ok(None);
    }
    let _ = build(root, tools, Mode::FastBoot)?;
    let base = initrd(root, tools)?;
    let mut archive =
        std::fs::read(&base).map_err(|e| format!("cannot read {}: {e}", base.display()))?;
    archive.truncate(archive.len().saturating_sub(2 * zoo_abi::tar::BLOCK));
    let wine =
        std::fs::read(&export).map_err(|e| format!("cannot read {}: {e}", export.display()))?;
    let mut files = 0;
    for e in zoo_abi::tar::read(&wine) {
        let e = e.map_err(|e| format!("{}: {e:?}", export.display()))?;
        let header = zoo_abi::tar::header(e.name, e.data.len())
            .ok_or_else(|| format!("cannot make a tar header for {}", e.name))?;
        archive.extend_from_slice(&header);
        archive.extend_from_slice(e.data);
        archive.resize(
            archive.len().div_ceil(zoo_abi::tar::BLOCK) * zoo_abi::tar::BLOCK,
            0,
        );
        files += 1;
    }
    // musl's loader search path, as Alpine keeps it, plus Wine's Unix
    // libraries: they name `ntdll.so` without a path, and musl only reuses
    // the copy Wine already loaded once its search finds the same file
    // (docs/linux10b-design.md).
    let path_file = b"/lib:/usr/local/lib:/usr/lib:/usr/lib/wine/x86_64-unix\n";
    let header = zoo_abi::tar::header("etc/ld-musl-x86_64.path", path_file.len())
        .ok_or("cannot make a tar header for etc/ld-musl-x86_64.path")?;
    archive.extend_from_slice(&header);
    archive.extend_from_slice(path_file);
    archive.resize(
        archive.len().div_ceil(zoo_abi::tar::BLOCK) * zoo_abi::tar::BLOCK,
        0,
    );
    archive.extend_from_slice(&[0; 2 * zoo_abi::tar::BLOCK]);
    for e in zoo_abi::tar::read(&archive) {
        e.map_err(|e| format!("wine initrd self-check failed: {e:?}"))?;
    }
    let out_dir = root.join("build").join("wine");
    let initrd_path = out_dir.join("initrd-wine.tar");
    std::fs::write(&initrd_path, &archive)
        .map_err(|e| format!("cannot write {}: {e}", initrd_path.display()))?;
    let elf = root
        .join("target")
        .join(format!("kernel-{}", Mode::FastBoot.name()))
        .join(KERNEL_TARGET)
        .join("release")
        .join(KERNEL_PACKAGE);
    let img = out_dir.join("zoo-os-wine-uefi.img");
    bootloader::UefiBoot::new(&elf)
        .set_ramdisk(&initrd_path)
        .create_disk_image(&img)
        .map_err(|e| format!("failed to create {}: {e:#}", img.display()))?;
    println!(
        "[build] wine image ({files} Wine files, initrd {} MB) -> {}",
        archive.len() >> 20,
        img.display()
    );
    Ok(Some(img))
}

/// The model image (docs/m5b-design.md §5): the normal kernel with an
/// initrd that also holds the model and `test/elephant.txt` (the prompt and
/// the token ids the host engine produced for it). Kept separate so the
/// other cases do not carry 430 MB each.
/// Image `mode` with the assistant's model added to its initial filesystem
/// (docs/m5c2-design.md; for the everyday image, docs/a3-design.md).
pub fn build_model_image(
    root: &Path,
    tools: &Tools,
    mode: Mode,
    model: &[u8],
    prompt: &str,
    expected: &[u32],
) -> Result<PathBuf, String> {
    let plain = build(root, tools, mode)?;
    let _ = plain;
    let base = initrd(root, tools)?;
    let mut archive =
        std::fs::read(&base).map_err(|e| format!("cannot read {}: {e}", base.display()))?;
    // Drop the two end-of-archive blocks, append, then end again.
    archive.truncate(archive.len().saturating_sub(2 * zoo_abi::tar::BLOCK));
    let ids: Vec<String> = expected.iter().map(u32::to_string).collect();
    let spec = format!(
        "{prompt}
{}
",
        ids.join(",")
    );
    for (name, data) in [
        ("models/qwen2.5-0.5b-instruct-q4_0.gguf", model),
        ("test/elephant.txt", spec.as_bytes()),
    ] {
        let header = zoo_abi::tar::header(name, data.len())
            .ok_or_else(|| format!("cannot make a tar header for {name}"))?;
        archive.extend_from_slice(&header);
        archive.extend_from_slice(data);
        archive.resize(
            archive.len().div_ceil(zoo_abi::tar::BLOCK) * zoo_abi::tar::BLOCK,
            0,
        );
    }
    archive.extend_from_slice(&[0; 2 * zoo_abi::tar::BLOCK]);
    let count = zoo_abi::tar::read(&archive)
        .map(|e| e.map_err(|e| format!("model initrd self-check failed: {e:?}")))
        .collect::<Result<Vec<_>, _>>()?
        .len();
    let out_dir = root.join("build").join(format!("{}-model", mode.name()));
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    let initrd_path = out_dir.join("initrd-model.tar");
    std::fs::write(&initrd_path, &archive)
        .map_err(|e| format!("cannot write {}: {e}", initrd_path.display()))?;
    let elf = root
        .join("target")
        .join(format!("kernel-{}", mode.name()))
        .join(KERNEL_TARGET)
        .join("release")
        .join(KERNEL_PACKAGE);
    let img = out_dir.join(format!("zoo-os-{}-model-uefi.img", mode.name()));
    bootloader::UefiBoot::new(&elf)
        .set_ramdisk(&initrd_path)
        .create_disk_image(&img)
        .map_err(|e| format!("failed to create {}: {e:#}", img.display()))?;
    println!(
        "[build] model image ({count} files, initrd {} MB) -> {}",
        archive.len() >> 20,
        img.display()
    );
    Ok(img)
}

/// Block 0 of the model disk starts with this, then the model's length
/// (8 bytes, little-endian); the kernel's `virtio_blk::MODEL_MAGIC`.
const MODEL_DISK_MAGIC: &[u8; 8] = b"ZOOMODL1";

/// The read-only model disk (docs/stage2-design.md §3):
/// `build/model-disk.img`, a 4 KiB header block, then the model padded to
/// whole 4 KiB blocks. Rewritten only when the model file is newer or a
/// different size.
pub fn model_disk(root: &Path) -> Result<PathBuf, String> {
    use std::io::{Read as _, Write as _};
    let model = crate::infer::model_path(root);
    let meta = std::fs::metadata(&model).map_err(|e| {
        format!(
            "cannot read {} ({e}); see tools/models/README.md",
            model.display()
        )
    })?;
    let len = meta.len();
    let out = root.join("build").join("model-disk.img");
    let blocks = len.div_ceil(4096);
    let fresh = (|| {
        let disk = std::fs::metadata(&out).ok()?;
        if disk.len() != (blocks + 1) * 4096 || disk.modified().ok()? < meta.modified().ok()? {
            return None;
        }
        let mut head = [0u8; 16];
        std::fs::File::open(&out).ok()?.read_exact(&mut head).ok()?;
        (&head[..8] == MODEL_DISK_MAGIC && head[8..16] == len.to_le_bytes()).then_some(())
    })()
    .is_some();
    if fresh {
        println!("[build] model disk (up to date) -> {}", out.display());
        return Ok(out);
    }
    // Checks the pinned SHA-256 before anything is written.
    let data = crate::infer::load_model(root)?;
    let mut head = vec![0u8; 4096];
    head[..8].copy_from_slice(MODEL_DISK_MAGIC);
    head[8..16].copy_from_slice(&len.to_le_bytes());
    let pad = vec![0u8; (blocks * 4096 - len) as usize];
    let tmp = out.with_extension("img.tmp");
    std::fs::create_dir_all(out.parent().unwrap_or(root))
        .map_err(|e| format!("cannot create build/: {e}"))?;
    let mut f =
        std::fs::File::create(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    f.write_all(&head)
        .and_then(|()| f.write_all(&data))
        .and_then(|()| f.write_all(&pad))
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, &out).map_err(|e| format!("cannot move {}: {e}", out.display()))?;
    println!("[build] model disk ({} MB) -> {}", len >> 20, out.display());
    Ok(out)
}
