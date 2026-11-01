# Linux step 10c (L10c): ZooFS 2, an on-disk file system for the home folder

Status: implemented 2026-11-01 (version 0.52.0). Follows L10b
(docs/linux10b-design.md). Replaces the snapshot store of S5
(docs/s5-design.md).

## 1. Why

L10b ran Wine with its prefix in `/tmp`, which lives in RAM. A prefix is
880 files and 167 MB, measured in ZooOS after `wine cmd`: almost all
of it full copies of Wine's DLLs, and Wine has no option to link them
instead. The largest is `shell32.dll`, 9.2 MB. Keeping the prefix in the
home folder, so it survives a reboot, failed on the old store:

- It held every file's bytes in BadgerHandler's memory.
- Each save rewrote the whole home folder as one snapshot.
- Each snapshot had to fit in half of a 16 MiB disk.

The owner chose a real on-disk file system over patching Wine to link its
DLLs (as Valve's Proton does): every persistent program will need one,
and some (World of Warcraft) need tens of GB.

L10c is the storage only. L10d puts Wine's prefix in the home folder. It
also needs symbolic links, `mmap` of home files, longer names and more
characters in names, none of which this step changes.

## 2. Layout

The disk is 4 KiB blocks. Blocks 0 and 1 are superblocks A and B; the
rest hold file data and metadata, anywhere.

| Superblock bytes | Contents |
| --- | --- |
| 0..8 | `ZOOFS002` |
| 8..16 | generation number |
| 16..24 | disk size in blocks (a disk of another size is not read) |
| 24..32 | metadata length in bytes |
| 32..36 | CRC-32 of the metadata |
| 36..40 | number of metadata runs |
| 40.. | runs: first block (8 bytes), count (4 bytes); at most 337 |
| 4092..4096 | CRC-32 of bytes 0..4092 |

**The metadata** is the `/home/user` tree, written depth-first:
- A version (4 bytes).
- Per entry: a kind, then the name as a 2-byte length and its bytes.
- A folder: its number of children, then the children.
- A file:
  - its size;
  - its blocks as runs (first block, count), where first 0 means a run
    of zeros;
  - the runs must cover the size exactly.

Loading checks every block number against the disk.

## 3. Saving: one crash-safe generation at a time

Changes collect in memory: the folder tree, and each changed 4 KiB block
of a file. A save runs once changes have been quiet for 0.5 s (as
before), or at once when 8 MiB of changed blocks are waiting. A save:

1. Writes each run of changed blocks to free blocks, consecutive where
   possible, up to 64 KiB per disk request.
2. Writes the metadata to free blocks.
3. Writes the superblock last, into the slot not holding the current
   generation. This single 4 KiB write is the commit point.

**Free** means used by neither the current generation nor the previous
one. A save therefore never overwrites a block that a superblock still
on the disk names:

- A save cut short (power loss, crash) leaves the current generation
  whole.
- A damaged newest generation falls back to the one before it, intact
  even if a later save was cut short after reusing blocks.

Each save allocates first-fit from the start of the disk. Data stays
compact, and blocks older generations freed are reused first.

**What fails the checks:** a superblock fails if its CRC is wrong or it
names another disk size. Its generation fails if the metadata's CRC is
wrong, the metadata does not parse, or a block is out of range. Either
way it is ignored, and the other superblock is used. The log lines keep
their S5 wording (`saved generation N to slot A`, `loaded generation N
from slot B`, `slot B: generation 2 failed its check, ignored`). The
verification cases and anyone reading the log see the same events.

**Durability.** ZooOS's virtio driver negotiates no write cache, so QEMU
completes a write only once it is on the host. Requests are synchronous,
one at a time, so writes reach the disk in order. A real disk driver
will need explicit flushes before and after the superblock.

## 4. Memory and speed

- **The folder tree stays in memory**: names, sizes, block maps. A file's
  map costs 8 bytes per 4 KiB block, so 2 MB for a 1 GiB file.
- **File data does not.** Reads of saved blocks go to the disk, in runs
  of up to 64 KiB, straight into the reader's shared buffer when they are
  block-aligned.
- **Changed blocks wait in a slab** of fixed 256 KiB chunks, with a per-file
  table from block index to slot. A first version allocated each block
  separately and kept them in a sorted map. It made 1 MiB writes about
  30 % slower than the old in-memory store (median of five runs). The slab
  removed that (§7 of the verification record has the numbers).
- **If saves keep failing** (a full disk), writes stop with "no space" at
  32 MiB waiting.
- **Limits:** files up to 4 GiB (was 16 MiB); 65,536 entries (was 4,096),
  with the slots of removed entries reused; test disks of 1 GiB (was
  16 MiB). The host file only grows as far as blocks are written.

New system calls `BLOCK_READ_RUN` / `BLOCK_WRITE_RUN` (62/63) move up to
16 consecutive blocks per request through a 64 KiB bounce buffer. The
three-argument system call ABI carries the count in bits 48..64 of the
block number.

## 5. Not done

- **No migration** from the S5 format (`ZOOFS001`): such a disk is logged
  (`older format (ZOOFS001), not read`) and starts from the seed. No
  owner data was on such disks; only test images were.
- **No per-operation journal.** Changes since the last save (at most about
  0.5 s, or 8 MiB) are lost on power loss, as before. There is no
  `fsync` yet.
- **Data blocks have no checksums.** Only the superblocks and the
  metadata do.
- **Not changed here:** names (64 bytes, letters, digits, space, `.`,
  `_`, `-`) and the protocol's 256-byte messages. L10d will change them
  for Wine.
- **Only virtio-blk** (QEMU) has a driver. USB storage, AHCI and NVMe for
  real machines come later.
