# Linux step 10d (L10d): Wine's prefix in the home folder

Status: implemented 2026-11-01 (version 0.53.0). Follows L10c
(docs/linux10c-design.md). **Wine now keeps its prefix in
`/home/user/.wine` on ZooFS 2.** The first start creates it (167 MB,
887 files); later boots find it on disk, so `wine cmd /c echo %OS%`
runs in seconds.

## 1. What the prefix needed

### Names

Wine's prefix uses names like:
- `dosdevices/c:`
- `Program Files (x86)`
- winsxs folders of 80+ bytes
- names with `{`, `}`, `+`, `,`, `~`

ZooOS allowed only letters, digits, space, `.`, `_` and `-`, up to 64
bytes. Now:

- **Names:** any UTF-8 without `/` or NUL, not `.` or `..`, up to 255
  bytes. A longer name gives `ENAMETOOLONG`, as on Linux.
- **Folders:** up to 128 deep.
- **Messages:** channel messages may be up to 1 KiB (they are stored at
  their own length, so small ones cost no more).
- **The file protocol:** a 2-byte path length, paths up to 1,000 bytes,
  directory-entry names up to 255 bytes.
- **Linux paths:** up to 4,096 bytes (`PATH_MAX`), copied to the heap
  rather than the 16 KiB kernel stack. They were capped at 256 bytes
  before, and a longer one gave `EFAULT`.

### Symbolic links in the home folder

- **Storage:** BadgerHandler stores links: a new node kind, kept in
  ZooFS 2 as a third metadata record (name, target). New requests:
  - `CreateLink`;
  - `ReadLink`;
  - `ListLinks`, which returns the `k`-th link below a folder as
    `path\0target`.
- **Following them:** the kernel follows links, as it does for `/tmp`
  (L10b). It reads all the home folder's links once, at a Linux
  program's first call, and keeps them in `Scheduler::home_links`.
  `symlink`, `unlink` and `rename` keep that copy current, so resolving
  a path costs no extra round trip to BadgerHandler.
- **Calls:** links are followed in every component, except where the
  call names the link itself:
  - `lstat` and `fstatat(AT_SYMLINK_NOFOLLOW)` report `S_IFLNK`;
  - `readlink`/`readlinkat`;
  - `unlink`, `rename`;
  - `getdents64` reports `DT_LNK`.
- **Limit:** links are made only through Linux calls. A ZooOS program
  (the Files app) that renames a folder holding links leaves the
  kernel's copy stale until the next boot. Nothing does that yet.

### `*at` calls relative to an open home folder

`openat`, `fstatat`, `faccessat`, `mkdirat`, `unlinkat`, `renameat`,
`readlinkat` and `symlinkat` used to ignore the folder descriptor outside
`/tmp`. They now resolve relative to it everywhere.

### `mmap` of home-folder files

- **A private copy:** the pages are mapped first, then filled through the
  64 KiB bulk buffer, one BadgerHandler reply at a time. Each reply makes
  the call block and run again; `Task::home_map` keeps the progress,
  keyed by the call's arguments, so an interrupted call does not resume
  into a different one.
- **Writable `MAP_SHARED`** is refused with `ENODEV`, because changes
  would not reach the file. Wine then reports the file system as unable
  to map files shared and writable; its prefix does not need that.
- **Use:** Wine maps DLLs from the prefix to read their resources (the
  `copy error 1812` messages before this).

### File times

`utime`, `utimes`, `futimesat`, `utimensat` are accepted for existing
files. No file system keeps times yet. Wine's installer sets them on
every file it copies, and logged `errno 38` for each before.

## 2. Gathering small writes

Wineserver saves the registry with stdio: musl's 1 KiB buffer meant
15,616 separate write requests for one prefix. The kernel now gathers a
process's consecutive small writes to one home-folder file into one
request of up to 64 KiB (`Proc::linux_wbuf`).

- **When it is sent:** before the process's next other system call. So
  its own reads, `close`, `rename`, `fork`, `exec` and exit see the data.
- **What it changes:** until then, another program reading the same file
  may not see the newest bytes. Programs' stdio buffers behave the same
  way.
- **If sending fails:** the error goes to the call that sent it.

## 3. Results

- **`wine-home-1`** (fresh disk, `--accel whpx`), 68.6 s for the whole
  case: Wine creates its prefix, `cmd.exe` prints `Windows_NT`, wineserver
  exits, BadgerHandler saves.
- **`wine-home-2`** (same disk), 11.5 s: generation 79 is loaded (887
  files, 41,727 blocks); `cmd.exe` prints `Windows_NT`.
- **Under TCG**, creating the prefix takes more than 8 minutes: Wine
  copies 167 MB at about 350 KB/s there. Both cases therefore run only
  with an accelerator, as `assistant-live` does. The second boot alone
  passes under TCG in 20-22 s. `wine-cmd` (prefix in `/tmp`) keeps
  covering Wine under TCG.

## 4. Not done

- **No graphics driver** for Wine yet; console programs only.
- **A `write` to a home file moves at most 64 KiB** and returns a short
  count, as since L4. Programs that loop (stdio, Wine) are unaffected.
- **No `AF_INET` sockets.**
