# Linux step 4 (L4): writable files in the home folder

Status: implemented 2026-10-31 (version 0.43.0). Follows L3 and F1
(docs/f1-design.md): Linux programs write in `/home/user`, which is
BadgerHandler's, through the same fast path native programs use.

## 1. Goal

`c/linux/fulmar-test.c` (Arctic fulmar) is ordinary Linux C: `mkdir`
(and `EEXIST`), `fopen` with `w`, `a` and `r`, 3000 bytes through raw
`write`/`read`, `lseek(SEEK_END)`, `rename`, `readdir`, `unlink`,
`ftruncate`, relative paths after `chdir`, and 1 MiB written and read back
with the time taken. ZooOS's own shell then `cat`s the file it left.

## 2. Design

- **The home folder through BadgerHandler.** A Linux program started by
  the shell's `linux` command gets a home-folder handle from the
  supervisor, as the desktop does. Paths under `/home/user` go to
  BadgerHandler; everything else stays the read-only initrd.
- **The kernel as BadgerHandler's client, inside a system call.** A call
  may need several requests in a row (`open` with `O_CREAT|O_TRUNC`: stat,
  create, truncate). Each task keeps the replies its current call has had.
  The call sends its next request and blocks; when the reply arrives the
  call runs again from the top, gets the earlier replies back in order
  without asking again, and continues. When the call finishes, the replies
  are dropped. This reuses ZooOS's existing "block and re-run" mechanism;
  nothing else in the kernel waits.
- **Bulk transfers (F1).** Each such program gets a kernel-owned 64 KiB
  shared buffer, sent to its BadgerHandler session at start-up. `read` and
  `write` move up to 64 KiB per request through it; the kernel copies
  between the program's memory and the buffer's pages directly.
- **Calls:** `open`/`openat` with `O_CREAT`, `O_EXCL`, `O_TRUNC`,
  `O_APPEND`, `O_DIRECTORY`; `read`, `readv`, `write`, `writev` (short
  writes allowed, as Linux allows); `lseek` (`SEEK_END` asks for the size);
  `getdents64` (one request per entry); `fstat`/`stat`/`newfstatat`
  (home files as `0o100644`, folders `0o040755`); `access`, `chdir`;
  `mkdir`/`mkdirat`, `rmdir`, `unlink`/`unlinkat` (with
  `AT_REMOVEDIR`), `rename`/`renameat`/`renameat2` (within the home
  folder; else `EXDEV`), `truncate`/`ftruncate`, `fsync`/`fdatasync`
  (accepted: BadgerHandler saves on its own schedule). BadgerHandler's
  errors become Linux ones (`ENOENT`, `EEXIST`, `ENOTEMPTY`, `ENOTDIR`,
  `EINVAL`, `EACCES`, `ENOSPC`, else `EIO`).

## 3. Verification

`linux-fulmar`: the program reports `/home/user/Fulmar: big.bin
notes.txt`, its 1 MiB timing and `all checks passed`, exits 0; then the
shell's `cat Fulmar/notes.txt` prints `line one` and `line two`.

**Measured** (WHPX, three runs each, 1 MiB through `write` and `read` in
the Linux program):

| | Message path (buffer off) | Shared buffer |
| --- | --- | --- |
| Write | 77.6–92.4 Mcycles | 6.4–8.0 Mcycles |
| Read | 44.7–57.4 Mcycles | 0.73–0.88 Mcycles |

Negative controls:

| Change | Result |
| --- | --- |
| Earlier replies not kept (no replay) | `FAIL` (timeout): multi-request calls never finish |
| `O_APPEND` ignored | `FAILED read back`, exit 24 |
| Bulk write leaves its last byte stale (but reports it written) | `FAILED read back`, exit 24 |

A first version of the last control wrote one byte less and reported it
honestly; that is a legal short write, the program wrote the rest, and the
case passed, as it should.

## 4. Not in step 4

Writing outside the home folder (the initrd stays read-only),
file-backed `mmap`, file locks, permissions beyond the owner, hot
re-reading of a file another program changes while open (no shared
cache), `fork`/`execve`, signals, threads.
