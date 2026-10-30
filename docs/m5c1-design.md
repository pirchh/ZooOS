# M5c-1 design: a writable filesystem, directory capabilities and the action broker

Status: implemented and verified 2026-10-30 (results in docs/verification.md;
§6 lists where the implementation differs from the first draft). Part of M5c
(the assistant); this step is everything the assistant's tools need
**except** the model, so it can be verified deterministically under software
emulation. M5c-2 adds the conversation UI and the model loop.

## 1. Filesystem (kernel, in memory)

- A tree of directories and files held in kernel memory (`kernel/src/fs.rs`):
  **`/bin`** (the initrd programs, read-only) and **`/home/user`**, seeded at
  boot from initrd entries under `home/user/` (the sample documents in
  `seed/home/user/`, packed by xtask), writable. Other initrd entries (the
  model, test data) stay reachable through `fs_map` only.
- Not persistent: contents are lost at shutdown. Persistent storage needs a
  disk driver and crash consistency (DECISIONS.md: document recovery before
  trusting personal data), which is deferred.
- Limits: names ≤ 64 bytes of `[A-Za-z0-9 ._-]`, paths ≤ 256 bytes, depth
  ≤ 16, ≤ 4096 nodes. Files are seeded only; there are no file writes yet.

## 2. Directory capabilities

- New handle kind **directory**, with rights `READ`, `CREATE`, `REMOVE`. A
  directory handle names a subtree; every path argument is **relative to it**.
- Paths are checked by the kernel: components separated by `/`; **empty, `.`
  and `..` components are rejected** (`INVAL`), as are absolute paths. A path
  therefore cannot leave the handle's subtree by construction. There are no
  symbolic links, so no link races; system calls run with interrupts off on
  one CPU, so check-then-act races cannot occur either.
- `dir_open(dir, path, rights)` returns a handle to a subdirectory with
  **rights no wider than the parent's** (`RIGHTS` otherwise).
- Where processes get them: a process started by the kernel (boot
  Zookeeper, session Zookeeper, a program the shell `run`s) may call
  `fs_root` once to receive a root handle; everyone else only gets the
  directory handles passed to them. `spawn_with` (below) passes two handles,
  so a service can get a channel and a directory.

## 3. New system calls (ABI version 4)

Calls 20–25 take the directory handle in `rdi` and a pointer to
`zoo_abi::FsArgs { path, path_len, arg, buf, buf_len }` in `rsi` (the system
call path passes three registers). The kernel copies the path out of user
memory before checking it.

| # | Call | `arg` / `buf` | Result |
| --- | --- | --- | --- |
| 19 | `fs_root()` | — | root directory handle with all rights (a process the kernel started, once; `RIGHTS` otherwise) |
| 20 | `dir_open` | rights / — | handle to the directory at `path`; rights must be a subset of the parent's (`RIGHTS`); `NOTDIR` for a file |
| 21 | `dir_list` | index / `DirEntry` | the `index`-th entry of the directory at `path` (name, kind, size), sorted by name; `NOTFOUND` past the end (needs `READ`) |
| 22 | `dir_stat` | — / `DirEntry` | kind and size of `path` (needs `READ`) |
| 23 | `file_read` | offset / bytes | bytes read (needs `READ`) |
| 24 | `dir_create` | — | new empty directory; parent must exist and be writable (needs `CREATE`; `EXISTS` if taken) |
| 25 | `dir_remove` | — | removes an **empty** directory, never the handle's own (needs `REMOVE`; `NOTEMPTY` otherwise) |
| 26 | `spawn_with(name ptr, len, handles ptr)` | — | like `spawn`, moving two handle values (`NO_HANDLE` for none) into the child, which gets them in `rdi` and `rsi` |

New errors: 15 `EXISTS`, 16 `NOTEMPTY`, 17 `NOTDIR`. Removed nodes are never
reused, so a handle to a removed directory only ever sees `NOTFOUND`. `/bin`
is read-only for every handle (`RIGHTS`).

## 4. The action broker: OtterHandler

A user process (`user/src/bin/otter-handler.rs`) that owns a directory
handle for `/home/user` (READ, CREATE, REMOVE) and serves tool requests over
a channel. **The model never gets a directory handle or system-call access
to files; only the broker does.**

| Tool | Behaviour |
| --- | --- |
| `search <text>` | names (and the first 64 KiB of files) under home containing `<text>`, case-insensitive; at most 20 results |
| `list <path>` | entries of a folder |
| `inspect <path>` | kind and size |
| `plan <path>` | what `create` would do (which folders are new), without changing anything |
| `create <path>` | creates the folder and any missing parents (≤ 8 levels); each new folder goes into the **journal** |
| `undo` | removes the folders the last `create` made, deepest first, **only if they are still empty**; never removes anything else |

Protocol: one request per message; the reply is zero or more `| <line>`
messages, then `ok` or `err <reason>`. Results are plain text with real
paths. Errors name the reason (`path escapes home`, `already exists`,
`not found`, `unknown tool`, …); `undo` reports `not empty, left in place:
<path>` for each folder it keeps. A `create` that makes nothing reports
`already exists` and leaves an empty journal. There is deliberately no tool
that deletes or changes files.

## 5. Verification

A new start-up check in Zookeeper (`check_directories`, deterministic,
software emulation), part of the `normal` case:

1. **Kernel rules, called directly:** a second `fs_root` (`RIGHTS`);
   `../bin`, `/bin` and `a//b` (`INVAL`); opening a file as a directory
   (`NOTDIR`); creating through a read-only handle and widening rights
   (`RIGHTS`); creating under `/bin` (`RIGHTS`); removing a non-empty folder
   (`NOTEMPTY`) and the handle's own folder (`INVAL`); reading the seeded
   recipe; a handle to a removed folder (`NOTFOUND`).
2. **Broker script:** Zookeeper starts OtterHandler with `spawn_with`
   (channel + home handle) and requires exact replies to 17 requests: name
   and content search, list, inspect, `plan` (and that it changed nothing),
   `create Projects/2026`, `undo` (both removed, deepest first), `undo`
   again (nothing to undo), an existing folder, two path escapes, an unknown
   tool, and a create after which Zookeeper adds `Projects/2027/Keep`
   itself: `undo` must leave both folders in place. Zookeeper then removes
   its own test folders, so home is unchanged.
3. The verifier requires `PROC start/exit pid=8 name=otter-handler code=0`
   and Zookeeper's exit 0. Any wrong reply makes Zookeeper exit with a
   distinct code (26 kernel rules, 27 broker script, 28 broker exit).
   Negative controls are in docs/verification.md.

## 6. Differences from the first draft

- Calls 20–25 take an argument struct instead of passing arguments 4–6 in
  `r10`, `r8` and `r9`.
- `spawn_with` takes a pointer to two handle values instead of a pointer and
  a count.
- The "content added later survives `undo`" check uses a folder Zookeeper
  creates, since there are no file writes yet; the escape checks use
  `../bin`, `/bin` and `a//b` rather than the draft's examples.
