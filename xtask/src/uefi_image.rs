//! UEFI disk images of any size (docs/usb-model-design.md).
//!
//! `bootloader` 0.11.17 sizes its FAT partition as the files' total plus
//! about 2 MiB, which leaves no room for the FAT tables once the files pass
//! a few hundred MB: a 1.7 GB ramdisk fails with "No space left on device".
//! So the bootloader crate only exports the files (its UEFI loader, the
//! kernel, the ramdisk, `boot.json`), and this module lays them out: a FAT
//! partition sized with room for its tables, inside a GPT disk with one
//! EFI system partition, as `bootloader`'s own `gpt.rs` does.

use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom};
use std::path::Path;

const MIB: u64 = 1 << 20;

/// Writes a bootable UEFI disk image at `img`: kernel `elf`, ramdisk
/// `initrd`, and `config` as `boot.json` when given.
pub fn create(
    elf: &Path,
    initrd: &Path,
    config: Option<&bootloader::BootConfig>,
    img: &Path,
) -> Result<(), String> {
    let staging = img.with_extension("files");
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|e| format!("cannot clear {}: {e}", staging.display()))?;
    }
    let mut b = bootloader::DiskImageBuilder::new(elf.to_path_buf());
    b.set_ramdisk(initrd.to_path_buf());
    if let Some(c) = config {
        b.set_boot_config(c);
    }
    b.create_uefi_tftp_folder(&staging).map_err(|e| {
        format!(
            "cannot export the boot files to {}: {e:#}",
            staging.display()
        )
    })?;
    // The exported names, and where UEFI and the loader look for them.
    let mut files = vec![("bootloader", "efi/boot/bootx64.efi")];
    for name in ["kernel-x86_64", "ramdisk", "boot.json"] {
        if staging.join(name).exists() {
            files.push((name, name));
        }
    }
    let fat = img.with_extension("fat");
    let r = fat_partition(&staging, &files, &fat).and_then(|()| gpt_disk(&fat, img));
    let _ = fs::remove_file(&fat);
    let _ = fs::remove_dir_all(&staging);
    r
}

/// A FAT partition at `out` holding `files` (exported name in `dir`, path
/// in the partition).
fn fat_partition(dir: &Path, files: &[(&str, &str)], out: &Path) -> Result<(), String> {
    let mut data = 0u64;
    for (name, _) in files {
        let len = fs::metadata(dir.join(name))
            .map_err(|e| format!("cannot read {name}: {e}"))?
            .len();
        // Each file ends in a partly used cluster (at most 32 KiB).
        data += len.div_ceil(32 << 10) * (32 << 10);
    }
    // FAT32's two tables take 8 bytes per 4 KiB cluster (0.2%): give
    // 1/128 for them, the reserved sectors and directories, plus 8 MiB.
    let size = (data + data / 128 + 8 * MIB).div_ceil(MIB) * MIB;
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(out)
        .map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    f.set_len(size)
        .map_err(|e| format!("cannot size {}: {e}", out.display()))?;
    fatfs::format_volume(
        &f,
        fatfs::FormatVolumeOptions::new().volume_label(*b"ZOOOS      "),
    )
    .map_err(|e| format!("cannot format {}: {e}", out.display()))?;
    let fs_ = fatfs::FileSystem::new(&f, fatfs::FsOptions::new())
        .map_err(|e| format!("cannot open {}: {e}", out.display()))?;
    let root = fs_.root_dir();
    for (name, to) in files {
        let mut parent = String::new();
        let parts: Vec<&str> = to.split('/').collect();
        for p in &parts[..parts.len() - 1] {
            if !parent.is_empty() {
                parent.push('/');
            }
            parent.push_str(p);
            root.create_dir(&parent)
                .map_err(|e| format!("cannot create {parent} on the FAT partition: {e}"))?;
        }
        let mut dst = root
            .create_file(to)
            .map_err(|e| format!("cannot create {to} on the FAT partition: {e}"))?;
        dst.truncate()
            .map_err(|e| format!("cannot truncate {to}: {e}"))?;
        let mut src = File::open(dir.join(name)).map_err(|e| format!("cannot open {name}: {e}"))?;
        io::copy(&mut src, &mut dst)
            .map_err(|e| format!("cannot copy {name} to {to} ({size} byte partition): {e}"))?;
    }
    Ok(())
}

/// A GPT disk at `out` with `fat` as its one EFI system partition.
fn gpt_disk(fat: &Path, out: &Path) -> Result<(), String> {
    let part = fs::metadata(fat)
        .map_err(|e| format!("cannot read {}: {e}", fat.display()))?
        .len();
    let mut disk = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(out)
        .map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    // Room for the primary and backup GPT headers and tables.
    let disk_size = part + 64 * 1024;
    disk.set_len(disk_size)
        .map_err(|e| format!("cannot size {}: {e}", out.display()))?;
    let mbr = gpt::mbr::ProtectiveMBR::with_lb_size(
        u32::try_from(disk_size / 512 - 1).unwrap_or(u32::MAX),
    );
    mbr.overwrite_lba0(&mut disk)
        .map_err(|e| format!("cannot write the protective MBR: {e}"))?;
    let block = gpt::disk::LogicalBlockSize::Lb512;
    let mut g = gpt::GptConfig::new()
        .writable(true)
        .initialized(false)
        .logical_block_size(block)
        .create_from_device(Box::new(&mut disk), None)
        .map_err(|e| format!("cannot create the GPT: {e}"))?;
    g.update_partitions(Default::default())
        .map_err(|e| format!("cannot set up the GPT: {e}"))?;
    let id = g
        .add_partition("boot", part, gpt::partition_types::EFI, 0, None)
        .map_err(|e| format!("cannot add the EFI partition: {e}"))?;
    let start = g
        .partitions()
        .get(&id)
        .ok_or("the EFI partition is missing after adding it")?
        .bytes_start(block)
        .map_err(|e| format!("cannot find the EFI partition's start: {e}"))?;
    g.write()
        .map_err(|e| format!("cannot write the GPT: {e}"))?;
    disk.seek(SeekFrom::Start(start))
        .map_err(|e| format!("cannot seek in {}: {e}", out.display()))?;
    io::copy(
        &mut File::open(fat).map_err(|e| format!("cannot open {}: {e}", fat.display()))?,
        &mut disk,
    )
    .map_err(|e| format!("cannot copy the FAT partition into {}: {e}", out.display()))?;
    Ok(())
}
