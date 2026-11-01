# Linux step 10a (L10a): Wine builds, starts and reaches its server

Status: implemented 2026-10-31 (version 0.50.0). Follows L9
(docs/linux9-design.md). This is the first half of L10. Wine 11.0 is built
from source for musl, packed into a ZooOS image, and run. `wine --version`
works. A real Windows program (`cmd.exe`) gets as far as Wine's
Windows-side start-up; the rest of L10 (§5) takes it from there.

## 1. Building Wine

- **Where:** in a WSL Alpine 3.24.3 environment (`zooos-build`), set up
  from the verified minimal file system (tools/alpine/README.md). The
  owner enabled WSL; it is a build tool only, and ZooOS is still tested
  only in QEMU.
- **What:** `tools/wine/build-wine.sh` builds the verified Wine 11.0
  source (tools/wine/README.md) as a lean 64-bit build: no X11, Wayland,
  Vulkan, audio, printing, scanners, cameras, Kerberos or TLS, and
  FreeType kept. The first attempt failed for lack of the kernel headers
  (`linux/cdrom.h`); Alpine's `linux-headers` was added.
- **Packing:** `tools/wine/export.sh` strips debug information, leaves out
  developer files (headers, man pages, import libraries `*.a`), copies
  links as files, adds `libgcc_s` and FreeType with its libraries, and
  writes `build/wine/wine-11.0-zooos.tar`: 958 files, 162 MB (910 MB
  unstripped).
- `*.sh` files keep LF line endings on every checkout (`.gitattributes`).

## 2. The `wine` image

`xtask::image::build_wine_image` builds the fast-boot kernel with the
normal initrd plus the exported files (initrd 164 MB). Only Wine cases use
it, so the other cases do not carry 164 MB. Wine cases report `SKIP`
without the export. New case `wine-version`: after Escape from the
desktop, `linux /usr/bin/wine --version` prints `wine-11.0` and exits 0.

Wine's programs name musl's libc as `libc.musl-x86_64.so.1`, Alpine's
name. No such file is needed: musl's loader treats any `libc.` name as
itself.

## 3. Kernel and system changes

- **Starting programs:**
  - A Linux program may be started by its absolute initrd path
    (`linux /usr/bin/wine`).
  - `execve` accepts any initrd file, not only `linux/<name>`.
  - `readlink("/proc/self/exe")` gives the running program's path; Wine
    finds its install directory this way.
- **Argument and environment strings are laid out as on Linux:** one
  block, in increasing order. ZooOS used to store them top-down, in
  reverse order. Wine's preloader slides `argv[1..]` down over `argv[0]`
  once `prctl(PR_SET_NAME)` succeeds, and with the reversed order its
  computed length was negative and wrapped, so it wrote 4 GiB below the
  stack. `skua-test` now checks the layout.
- **`/tmp`** for Linux programs is the home folder's `.tmp`, which
  BadgerHandler creates at start. Home-folder folders and files report
  owner-only permissions (`0700`/`0600`), which is accurate with one
  user, and `wineserver` requires it of its folders.
- **Hidden names.** Names starting with `.` (`.tmp`, Wine's `.wine`) are
  hidden by the Files app and OtterHandler's listings, as macOS's Finder
  hides them. A boot check first failed on `.tmp`, which is how this
  came up. Linux `readdir` still lists them, as on Linux.
- **New Linux calls:**
  - Process properties: `prctl` (names, parent-death signal,
    dumpability), `getrlimit`/`prlimit64`/`setrlimit` (64 descriptors,
    an 8 MiB stack, the rest unlimited; new limits are not kept),
    `setsid`/`getsid`/`setpgid`/`getpgid`/`getpgrp` (one session and
    group per process), `umask` (95).
  - Files: file locks (`fcntl` `F_GETLK`/`F_SETLK`/`F_SETLKW` and their
    open-file-description forms, `flock`: granted), `chmod`/`fchmod`/
    `fchmodat` (accepted for existing files), `statfs`/`fstatfs`,
    `fchdir`, and `pread64`/`pwrite64` for home-folder files.
  - A first version put `umask` on 90, which is `chmod` on x86-64. A
    `skua-test` check exposed it.
- **Socket names are visible to `stat`.** A bound Unix socket's path
  `stat`s as a socket (`S_ISSOCK`), and `unlink` frees the name.
  Wine's client checks the server's socket with `lstat` before it
  connects. `ermine-test` checks this.

## 4. musl's `libc.so`, corrected

L8's `libc.so` linked Zig's compiler runtime for three complex-number
helpers (`__mulsc3`, `__muldc3`, `__mulxc3`). That runtime is a single
object that also carries hidden copies of `memcpy`, `memset` and
`strlen`. Linking it left musl's own versions out of the exported
symbols. L8's programs never imported those functions from libc, so
nothing failed then; `wine` does, and stopped with `Error relocating
/usr/bin/wine: memcpy: symbol not found`.

The helpers now come from `c/musl/complex-mul.c`, written from ISO C
Annex G.5.1, and no compiler runtime is linked. The build revision is
in the cache name (`build/musl-1.2.5-b2/`), so the broken build is never
reused. `libc.so` exports 1,651 symbols, up from 1,586, and is 726 KB.
Each source is now compiled on its own (1,348 objects); this turned out
not to be the cause, but is easier to check.

## 5. What `wine cmd /c echo %OS%` shows (the rest of L10)

1. `wine` re-executes itself as `wine-preloader`, which works at
   `0x7d7d00000000` (L9's layout).
2. It creates `/home/user/.wine`, then forks and starts `wineserver`,
   which daemonizes and binds its socket.
3. `wine` connects to the server, and Wine's Windows side starts.
4. It stops at `err:virtual:virtual_map_user_shared_data failed to remap
   the process USD`.

The server makes its shared memory from temporary files in its folder:
it creates one, sizes it, unlinks it while keeping the descriptor, and
maps it shared, as every client does. The remaining work:

- **A RAM file system (tmpfs) for `/tmp`, and `memfd_create`:**
  unlinked-but-open files and true `MAP_SHARED` mappings between
  processes.
- **`mmap` of home-folder files** (private copies), for files in the
  Wine prefix.
- **Symbolic links in the home folder** (BadgerHandler), for
  `dosdevices/c:`.

## 6. Verification

- `wine-version` passes: `[10:wine] wine-11.0`, exit 0.
- New checks in existing cases:
  - `linux-skua`: argument and environment layout, `getrlimit`, `umask`.
  - `linux-fulmar`: `pwrite`/`pread` at an offset, `fchdir`, `statfs`.
  - `linux-ermine`: `lstat` of a bound socket.

Negative controls (each restored after):

| Change | Result |
| --- | --- |
| Environment strings placed before the arguments | `FAIL linux-skua`: exit 6, `environment layout` |
| Bound socket names invisible to `stat` | `FAIL linux-ermine`: exit 19, `lstat of a bound socket` |
| The libc alias as a copy instead of a hard link | **`OK`**: the claim that it had to be a link was wrong (musl never opens it); the alias and the tar hard-link support added for it were removed |

Two failures came up during the work rather than as planned controls:
- With `.tmp` in listings, the boot check `list` failed: `Zookeeper
  code=27`.
- With L8's `libc.so`, `wine` failed to relocate `memcpy` (exit 127).
