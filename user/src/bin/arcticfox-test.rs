//! ArcticFoxTest: the writable-file test (docs/posix4-design.md §4).
//! Started by Zookeeper with a home directory handle (all rights); exits 0
//! only if every check passes (the exit code names the first failure) and
//! leaves home exactly as it found it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use zoo_rt::{
    Error, dir_create, dir_open, dir_remove, dir_rights as r, dir_stat, file_create, file_read,
    file_remove, file_truncate, file_write, rename,
};

zoo_rt::entry2!(main);

fn main(home: u64, root: u64) -> u8 {
    match checks(home, root) {
        Ok(()) => {
            let _ = zoo_rt::log("ArcticFoxTest: all file checks passed");
            0
        }
        Err((code, what)) => {
            zoo_rt::log!("ArcticFoxTest: check {} failed: {}", code, what);
            code
        }
    }
}

fn check(ok: bool, code: u8, what: &'static str) -> Result<(), (u8, &'static str)> {
    if ok { Ok(()) } else { Err((code, what)) }
}

fn read_all(dir: u64, path: &str) -> Option<Vec<u8>> {
    let size = dir_stat(dir, path).ok()?.size as usize;
    let mut buf = vec![0u8; size];
    let n = file_read(dir, path, 0, &mut buf).ok()?;
    (n == size).then_some(buf)
}

fn checks(home: u64, root: u64) -> Result<(), (u8, &'static str)> {
    // 1. Create, write, read back.
    check(dir_create(home, "Scratch").is_ok(), 40, "create folder")?;
    check(
        file_create(home, "Scratch/a.txt").is_ok(),
        40,
        "create file",
    )?;
    check(
        file_create(home, "Scratch/a.txt") == Err(Error::Exists),
        40,
        "duplicate refused",
    )?;
    check(
        file_write(home, "Scratch/a.txt", 0, b"hello") == Ok(5),
        40,
        "write",
    )?;
    check(
        read_all(home, "Scratch/a.txt").as_deref() == Some(b"hello"),
        40,
        "read back",
    )?;

    // 2. Writing past the end leaves a zero gap.
    check(
        file_write(home, "Scratch/a.txt", 10_000, b"!") == Ok(1),
        41,
        "sparse write",
    )?;
    let all = read_all(home, "Scratch/a.txt").ok_or((41, "read"))?;
    check(all.len() == 10_001, 41, "size after sparse write")?;
    check(
        all[5..10_000].iter().all(|&b| b == 0) && all[10_000] == b'!',
        41,
        "gap reads as zero",
    )?;

    // 3. A 100 KiB write across many pages, read back at odd offsets.
    let big: Vec<u8> = (0..100 * 1024u32)
        .map(|i| (i * 7 + i / 4096) as u8)
        .collect();
    check(
        file_create(home, "Scratch/big.bin").is_ok(),
        42,
        "create big",
    )?;
    check(
        file_write(home, "Scratch/big.bin", 0, &big) == Ok(big.len()),
        42,
        "big write",
    )?;
    let mut part = [0u8; 5000];
    check(
        file_read(home, "Scratch/big.bin", 4093, &mut part) == Ok(5000),
        42,
        "read across pages",
    )?;
    check(part[..] == big[4093..9093], 42, "big contents")?;

    // 4. Truncate shrinks and zero-fills on regrowth.
    check(
        file_truncate(home, "Scratch/a.txt", 3).is_ok(),
        43,
        "truncate",
    )?;
    check(
        read_all(home, "Scratch/a.txt").as_deref() == Some(b"hel"),
        43,
        "truncated contents",
    )?;
    check(
        file_truncate(home, "Scratch/a.txt", 4096).is_ok(),
        43,
        "regrow",
    )?;
    let all = read_all(home, "Scratch/a.txt").ok_or((43, "read"))?;
    check(
        &all[..3] == b"hel" && all[3..].iter().all(|&b| b == 0),
        43,
        "regrown tail is zero",
    )?;

    // 5. A seeded file is copied on first write; restoring it leaves it as
    //    it was (other start-up checks and the desktop read it later).
    let original = read_all(home, "Documents/notes.txt").ok_or((44, "read seeded"))?;
    let extra = b"Remember: the arctic fox was here.\n";
    let at = original.len() as u64;
    check(
        file_write(home, "Documents/notes.txt", at, extra) == Ok(extra.len()),
        44,
        "append to seeded",
    )?;
    let changed = read_all(home, "Documents/notes.txt").ok_or((44, "read"))?;
    check(
        changed.starts_with(&original) && changed.ends_with(extra),
        44,
        "seeded file updated",
    )?;
    check(
        file_truncate(home, "Documents/notes.txt", at).is_ok(),
        44,
        "restore seeded",
    )?;
    check(
        read_all(home, "Documents/notes.txt") == Some(original),
        44,
        "seeded file restored",
    )?;

    // 6. Rename: files and folders, with refusals.
    check(
        rename(home, "Scratch/a.txt", "Scratch/b.txt").is_ok(),
        45,
        "rename file",
    )?;
    check(
        dir_stat(home, "Scratch/a.txt") == Err(Error::NotFound),
        45,
        "old name gone",
    )?;
    check(
        read_all(home, "Scratch/b.txt").map(|v| v.len()) == Some(4096),
        45,
        "new name has contents",
    )?;
    check(
        rename(home, "Scratch/b.txt", "Scratch/big.bin") == Err(Error::Exists),
        45,
        "overwrite refused",
    )?;
    check(
        rename(home, "Scratch", "Scratch2").is_ok(),
        45,
        "rename folder",
    )?;
    check(dir_create(home, "Scratch2/sub").is_ok(), 45, "create sub")?;
    check(
        rename(home, "Scratch2", "Scratch2/sub/x") == Err(Error::Inval),
        45,
        "move into own subtree refused",
    )?;
    check(
        rename(home, "Scratch2/b.txt", "../b.txt") == Err(Error::Inval),
        45,
        "escape refused",
    )?;

    // 7. Rights: a read-only handle and /bin.
    let ro = dir_open(home, "", r::READ).map_err(|_| (46, "read-only handle"))?;
    check(
        file_write(ro, "Scratch2/b.txt", 0, b"x") == Err(Error::Rights),
        46,
        "write needs WRITE",
    )?;
    check(
        file_create(ro, "Scratch2/c.txt") == Err(Error::Rights),
        46,
        "create needs CREATE",
    )?;
    check(
        file_remove(ro, "Scratch2/b.txt") == Err(Error::Rights),
        46,
        "remove needs REMOVE",
    )?;
    let _ = zoo_rt::close(ro);
    check(
        file_write(root, "bin/zookeeper", 0, b"x") == Err(Error::Rights),
        46,
        "/bin is read-only",
    )?;
    check(
        file_remove(root, "bin/zookeeper") == Err(Error::Rights),
        46,
        "/bin files stay",
    )?;
    check(
        file_write(home, "Scratch2/b.txt", 16 << 20, b"x") == Err(Error::Inval),
        46,
        "16 MiB file limit",
    )?;

    // 8. Clean up: home must be as it was.
    for f in ["Scratch2/b.txt", "Scratch2/big.bin"] {
        check(file_remove(home, f).is_ok(), 47, "remove file")?;
    }
    check(
        dir_remove(home, "Scratch2") == Err(Error::NotEmpty),
        47,
        "folder not empty yet",
    )?;
    check(dir_remove(home, "Scratch2/sub").is_ok(), 47, "remove sub")?;
    check(dir_remove(home, "Scratch2").is_ok(), 47, "remove folder")?;
    check(
        dir_stat(home, "Scratch2") == Err(Error::NotFound),
        47,
        "gone",
    )?;
    Ok(())
}
