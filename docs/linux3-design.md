# Linux step 3 (L3): folders, the working directory, uname and an environment

Status: implemented 2026-10-31 (version 0.41.0). Follows L2
(docs/linux2-design.md). Writable files in the home folder need the kernel
to talk to BadgerHandler during a system call, a design step of its own,
so they come in L4; this step covers what programs ask about their
surroundings.

## 1. Goal

`c/linux/skua-test.c` (Arctic skua) is ordinary Linux C that calls
`uname`, reads `HOME` and `USER`, lists `/home/user` with
`opendir`/`readdir`, `stat`s a folder and a file, `chdir`s and opens a
file by a relative path, goes back up with `..`, and uses `access`,
`getuid` and `getgid`. It runs unmodified.

## 2. Design

- **Folders in the initrd.** The initrd is a flat archive of paths; a
  folder is the root or any prefix of a packed path
  (`home/user/Documents`). `open`/`openat` of a folder gives a folder
  descriptor (`O_DIRECTORY` on a file is `ENOTDIR`); `read` on it is
  `EISDIR`. `getdents64` lists `.`, `..`, then the folder's entries in name
  order (`DT_DIR`/`DT_REG`), as many as fit, continuing on the next call;
  `lseek(fd, 0, SEEK_SET)` rewinds. `fstat`/`stat`/`lstat`/`newfstatat`
  report folders as `0o040555` and files as `0o100444`, owned by uid and
  gid 1000.
- **Working directory** per process (starts at `/`): `chdir`, `getcwd`;
  every path is resolved against it with `.` and `..` applied (L2 refused
  `..`).
- **`uname`** reports `Linux`, `zoo`, `6.1.0-zooos`, `ZooOS <version>`,
  `x86_64`: other kernels' Linux layers report Linux too, so programs that
  check take their Linux paths. The release and version name ZooOS.
- **Environment** on the start-up stack: `HOME=/home/user`,
  `PATH=/linux`, `USER=user`, `LANG=C.UTF-8`, `TERM=dumb`.
- **Small calls:** `access`/`faccessat` (existence; `W_OK` is `EROFS`),
  `getuid`/`getgid`/`geteuid`/`getegid` (1000), `getppid` (0), `fcntl`
  (`F_GETFD`/`F_SETFD`/`F_GETFL`/`F_SETFL`; musl's `opendir` uses it).

## 3. Verification

`linux-skua` case: `uname Linux x86_64`, `HOME=/home/user USER=user`,
`/home/user: Documents Music Photos`, `cwd /home/user;
Documents/notes.txt starts "Shopping: flour, eggs, milk."`, `all checks
passed`, exit 0, no unsupported calls (the first run showed `fcntl`, then
added). `linux-hello` and `linux-eider` pass unchanged.

Negative controls:

| Change | Result |
| --- | --- |
| No `.` and `..` in listings | `FAILED readdir . and ..`, exit 21 |
| `..` not applied in paths | `FAILED chdir ..`, exit 34 |
| Empty environment | `FAILED environment`, exit 11 |

## 4. Not in step 3

Writing, creating, renaming or removing files (L4, through
BadgerHandler), symbolic links, permissions beyond read-only, `fork`/
`execve`, signals, threads.
