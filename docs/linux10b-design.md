# Linux step 10b (L10b): `/tmp` in RAM, shared memory, and Wine's start-up

Status: implemented 2026-11-01 (version 0.51.0). Follows L10a
(docs/linux10-design.md). L10a left Wine stopped at its shared memory
(`virtual_map_user_shared_data failed`).

L10b gives Linux programs a RAM file system for `/tmp` with real shared
mappings, then fixes the further gaps Wine's start-up ran into, one at a
time. **Result:** `wine cmd /c echo %OS%` runs Wine's `cmd.exe`, a
Windows program, which prints `Windows_NT` (§7).

## 1. tmpfs: `/tmp` in RAM

`/tmp` is no longer the home folder's `.tmp` (L10a's stopgap, removed with
BadgerHandler's `.tmp` folder). The kernel keeps it:

- **Nodes.** `Scheduler.tmpfs` maps each full path (`tmp/…`) to a folder,
  a file or a symbolic link; `/tmp` itself is implicit (mode `1777`).
- **Files.** A file's contents are whole pages (`TmpData`), shared through
  a reference count by its name, its open descriptors and its mappings.
  The pages are freed when the last of these goes, so a file unlinked
  while open lives on, as wineserver needs.
- **Calls.**
  - By path: `open`/`openat`, the `stat` family, `access`, `chdir`,
    `mkdir`, `rmdir`, `unlink`, `rename`, `symlink`, `readlink`,
    `truncate`, `chmod`, `statfs`.
  - By descriptor: `read`/`readv`/`pread64`, `write`/`writev`/`pwrite64`,
    `lseek`, `getdents64` (with `DT_LNK`), `fstat`, `ftruncate`,
    `fallocate`, `fchdir`, `mmap`.
- **`memfd_create`** gives a tmpfs file without a name. Seals
  (`F_ADD_SEALS`/`F_GET_SEALS`) are accepted and not kept.
- **Not kept:** owners, permissions (every node is the one user's), times.

## 2. Paths: `*at` calls and symbolic links

One resolver, `linux_path_at`, now serves every path:

- **Relative names:** resolved against the working folder, or against an
  open folder's descriptor for the `*at` calls.
- **Symbolic links:** tmpfs links are followed in every component, the
  last one too unless the call says not to (`lstat`, `readlink`,
  `unlink`, `rename`, `AT_SYMLINK_NOFOLLOW`). A relative target is
  resolved against the link's folder; more than 40 links gives `ELOOP`.
- **Scope:** links exist only in tmpfs for now. `symlink` elsewhere fails
  (`EPERM` in the home folder, `EROFS` in the initrd).

## 3. Shared mappings

- **`MAP_SHARED` of a tmpfs file** maps the file's own pages, so every
  process mapping it, and `read`/`write` on it, see the same memory.
  - Each process records these ranges (`linux_shared`).
  - `munmap` and `MAP_FIXED` unmap such pages without freeing them, and
    so do `exec` and process exit.
  - `fork` maps the same pages in the child.
- **`MAP_PRIVATE` is copy-on-write.**
  - The mapping uses the file's own pages, read-only in the page tables,
    so it shows the file's later changes, as on Linux.
  - The first write to a page (when the mapping allows writing) copies
    that page alone. This happens in the page-fault handler, or in
    `linux_commit` when the kernel writes there for a call such as
    `read`.
  - `mprotect` records the new access; pages not yet copied stay
    read-only, so a write still copies them.
  - Wine maps read-only views of files as `MAP_PRIVATE` with read and
    write access and relies on this. A first version copied at `mmap`
    time, so Wine's processes never saw window classes and desktops the
    server added later (`Session object id doesn't match`). Every
    process then failed to find the desktop window and started another
    `explorer.exe /desktop`, until fork ran out of room.

## 4. Other gaps Wine's start-up exposed

- **GS base.** Wine's Windows code reaches its thread block (TEB) through
  `GS`. `arch_prctl(ARCH_SET_GS/ARCH_GET_GS)` now sets and reads a
  per-thread GS base. It is loaded on every switch, inherited by `clone`
  and `fork`, and cleared by `exec`; the kernel never uses `GS`. Without
  it, every Windows thread faulted at address `0x30` and was restarted
  forever.
- **Distinct folder inode numbers.** Initrd folders all reported inode 0.
  Wine finds the drive a Unix path lies on by comparing a path's folders
  with each drive's (device, inode), so `/usr/lib/wine` matched `/`
  (drive `Z:`) and Wine looked for its DLLs in `Z:\x86_64-windows`.
  Folders now get a hash of their full path, as tmpfs and home-folder
  nodes do.
- **musl's library path in the `wine` image.**
  - Wine's Unix libraries name `ntdll.so` without a path. musl reuses
    the copy already loaded only when its search finds that same file.
  - The `wine` image therefore carries `/etc/ld-musl-x86_64.path` with
    `/usr/lib/wine/x86_64-unix` added to musl's default path.
- **Descriptors.** 1,024 per process (was 64): Wine keeps one open per
  loaded library and failed with "too many open files". The close-on-exec
  and non-blocking flags became 1,024-bit sets.
- **Tasks.** Up to 512 threads at once (was 32): Wine's start-up runs
  about a dozen processes with several threads each, and `fork` failed
  with `EAGAIN`. Only used slots have their 16 KiB stacks mapped.
- **Kernel heap.** 8 MiB (was 1 MiB, the M1 size). The processes,
  descriptors and `/tmp` nodes Wine creates live there. This was raised
  before running out rather than after: a full heap stops the kernel.
- **Device numbers.** `/dev/null` and the console both reported device
  number 0. Wine compares stderr's number with `/dev/null`'s and prints
  nothing at all when they match, which hid every Wine message while
  this was being debugged. Devices now report Linux's numbers: 1,3 for
  `/dev/null`, 1,5, 1,9, and 5,1 for the console.
- **Devices:** `/dev/null`, `/dev/zero`, `/dev/urandom` (and
  `/dev/random`).
- **New Linux calls:**
  - `getrandom`, `sysinfo`, `getrusage` (all zero: not measured);
  - `clock_getres`, `setpriority`/`getpriority`, `sched_setaffinity`;
  - extended attributes: `EOPNOTSUPP`, after which Wine keeps DOS
    attributes itself.
- **Random numbers.** Without RDRAND (QEMU's `qemu64` CPU has none) they
  come from a SplitMix64 sequence seeded from the clock and mixed with
  the time-stamp counter. That is enough for hash seeds and temporary
  names, and **not cryptographically strong**. With RDRAND, its output is
  mixed in.

## 5. The shell no longer starves

The kernel task, which runs the shell, gets a turn only when it is not
idle. Nothing marked it busy when a key arrived, so a program that never
blocked kept the shell from seeing the key; this came up while a Wine
thread was faulting in a loop. Keyboard and mouse input now mark the
kernel task busy (`kernel_has_input`). `loon-test` spins forever, and the
case checks that `ps` still answers.

## 6. The shell's `linux` command takes environment variables

Leading `NAME=VALUE` words set or replace variables of the standard
environment:

```
linux WINEPREFIX=/tmp/wine /usr/bin/wine cmd /c echo %OS%
```

## 7. Where Wine is now

`linux WINEPREFIX=/tmp/wine /usr/bin/wine cmd /c echo %OS%` on the `wine`
image, under TCG:

1. Wine creates the prefix in `/tmp/wine` and starts its services:
   - `wineboot --init`, `services.exe`;
   - `rundll32` installing `wine.inf`;
   - `explorer.exe /desktop`.
2. Then it runs `cmd.exe`, which prints `Windows_NT`. `wine` exits with
   code 0.
3. The whole run takes 176-260 s in the runs recorded, almost all of it creating the
   prefix. The prefix lives in RAM, so every boot creates it again.

What is still missing:

- **No graphics driver.** This lean build has no X11 or Wayland, so
  explorer reports `no driver could be loaded` and runs without windows.
  Console programs work; programs with windows need a ZooOS display
  driver for Wine (a later step).
- **Network sockets** (`AF_INET`): `EAFNOSUPPORT`.
- **Not kept:** extended attributes, file locks between processes, and
  per-process CPU time.
- **A persistent prefix in the home folder** needs `mmap` of home-folder
  files and symbolic links there (L10c).
