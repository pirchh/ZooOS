# Services step 4 (S4): the filesystem as a service (BadgerHandler)

Status: implemented and verified 2026-10-30 (results in docs/verification.md;
§6 lists where the implementation differs from this plan). Undoes the
shortcut taken in
M5c-1 and POSIX step 4, where the filesystem was put inside the kernel
(DECISIONS.md), and matches ARCHITECTURE.md's "Filesystem service".
BadgerHandler: badgers dig burrows of tunnels and chambers, like folders.

## 1. Goal

- The file tree (folders, files, seeding from the initrd, copy-on-write of
  seeded files) moves from `kernel/src/fs.rs` into a user-space service.
  A bug in it then crashes one supervised process, not the kernel, and its
  memory comes from its own heap instead of the 1 MiB kernel heap.
- **Programs do not change.** The `zoo_rt` file functions keep their
  signatures; a directory handle becomes a channel to BadgerHandler instead
  of a kernel object.

## 2. Directory handles become channels

- Each directory handle is a channel; BadgerHandler remembers, per channel,
  the folder it names and its rights (READ, CREATE, REMOVE, WRITE). The
  same path rules as today: relative only, no `.`/`..`/empty components.
- `dir_open` asks BadgerHandler, which creates a new channel for the
  narrower folder and moves the client end back with `send_handle` (S3).
- Requests and replies are small messages (`list`, `stat`, `read`,
  `create`, `write`, `truncate`, `remove`, `rename`). File data larger than
  one message (256 bytes) goes in several messages; a later step can share
  pages instead for speed.
- `fs_root` and the kernel calls 19–25 and 41–45 are removed (ABI
  version 12). The kernel no longer has a filesystem; `fs_map`/`fs_stat`
  on the initrd stay for loading programs and the model.

## 3. Who starts it

- **Boot checks:** Zookeeper (check mode) starts its own BadgerHandler for
  the file tests (arcticfox-test, the OtterHandler script) and stops it
  afterwards, so the kernel's leak check stays exact.
- **Resident:** Zookeeper (supervise mode) starts BadgerHandler first,
  keeps the root channel, and hands narrower channels to services that
  need files.
- **Desktop:** `desktop` becomes a supervisor request. The resident
  Zookeeper starts the session and gives it a home-folder channel, which
  also puts the desktop under supervision (an S1 gap).

## 4. Failure

If BadgerHandler crashes, the supervisor restarts it; its in-memory state
is lost (as on reboot today) and clients see `PEERCLOSED` on their folder
channels. Persistence (S5) changes this.

## 5. Verification (as implemented)

See §6 for the differences; the checks that ran are listed in
docs/verification.md.

## 5a. Verification (as planned)

- Every existing file test must pass unchanged on the new implementation:
  arcticfox-test, the OtterHandler script and capability checks in
  Zookeeper, the `assistant`, `assistant-model` and `desktop` cases.
- New checks: the kernel no longer answers the removed calls (`NOSYS`); a
  BadgerHandler crash is restarted by the supervisor and a client's folder
  channel reports `PEERCLOSED`; the boot leak check still holds.
- Negative controls: BadgerHandler ignoring rights (a read-only folder
  channel can create), and path escapes accepted.

## 6. As implemented

- **Protocol** (`user/src/fsproto.rs`): request `op u8 | arg u64 |
  count u16 | path_len u8 | path | data`; reply `status i64 | bytes`. File
  data moves in pieces of at most 248 bytes per reply (reads) or what fits
  after the path (writes). `zoo_rt`'s file functions keep their old
  signatures; one request at a time per folder handle.
- **Limits raised** for this: handles per process 16 → 64 and `wait_any`
  16 → 64 (BadgerHandler holds one channel end per open folder; at most 62
  folders open at once).
- **Kernel:** `kernel/src/fs.rs` removed; calls 19–25 and 41–45 return
  `NOSYS`; the boot leak check no longer needs the file-page audit. ABI
  version 13. `FsArgs` was removed from `zoo-abi`.
- **Desktop home folder:** the shell's `desktop` asks the resident
  supervisor for a home-folder handle (`home` request); the supervisor opens
  one from BadgerHandler's root and sends it as a handle message; the kernel
  keeps it and gives it to the session's Zookeeper as its second start-up
  handle. The desktop itself is not yet a supervised service.
- **Supervisor:** `etc/services` lists `badger-handler` first; Zookeeper
  asks each new BadgerHandler for its root and logs `filesystem root from
  badger-handler (pid N)`. New shell command `restart <service>` (kills it;
  the restart policy brings it back).
- **Checks:** every earlier file test passes unchanged (ArcticFoxTest, the
  folder-permission checks, the OtterHandler script, Files, the Assistant);
  the old call returns `NOSYS`; the private BadgerHandler exits cleanly;
  `restart badger-handler` gives a new pid and a new root.

- **Not automated:** a client holding a folder handle while BadgerHandler
  dies (it would see `PEERCLOSED`). The restart itself is tested; nothing
  resident holds folder handles across it except Zookeeper's root, which is
  replaced.
