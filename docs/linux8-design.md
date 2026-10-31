# Linux step 8 (L8): dynamically linked programs

Status: implemented 2026-10-31 (version 0.48.0). Follows L7
(docs/linux7-design.md). Most Linux software, Wine included, is
dynamically linked: the kernel starts a dynamic loader named by the
program (`PT_INTERP`), which maps shared libraries with `mmap`, relocates
everything and runs constructors.

## 1. Goal

`c/linux/dovekie-test.c` (the dovekie is an Arctic seabird) is an ordinary
dynamically linked musl program. It needs `libdovekie.so`, and it checks:
- the library's functions, an exported global, and a constructor having
  run;
- thread-local storage in the library, separate per thread;
- the library calling `snprintf` in libc;
- `dlopen` of `libdovekie-plugin.so`, which calls back into `libdovekie`,
  and `dlerror` for a missing library;
- timings: starting the static `hello` against the dynamic `hello-dyn`.

## 2. Design

### musl's loader, built from source

- musl's `libc.so` is also its dynamic loader. Zig's bundled musl leaves
  the loader out (`ldso/dynlink.c`). So the owner approved downloading
  musl 1.2.5, which was checked three ways (tools/musl/README.md).
- `xtask/src/musl.rs` follows musl's Makefile and configure for x86_64
  and clang:
  - **Generated headers:** `bits/alltypes.h` (a Rust port of
    `mkalltypes.sed`), `bits/syscall.h` and `version.h`.
  - **Sources:** `src/*`, `src/malloc/mallocng` and `ldso`; an x86_64
    file replaces a generic one of the same name.
  - **Compiler flags:** configure's, with `-fPIC`.
  - **Link:** LLD is used directly (`zig ld.lld`), because Zig's `cc`
    driver drops these options. The link uses `-shared -e _dlstart
    --dynamic-list=dynamic.list --exclude-libs=ALL --no-undefined
    --hash-style=both --strip-all`, plus Zig's compiler runtime, built
    from the pinned toolchain's source (configure's `-lcompiler_rt`;
    musl needs it for complex multiplication).
- The result, 865 KB, is cached in `build/musl-1.2.5/` and packed as
  `lib/ld-musl-x86_64.so.1`.
- Shared libraries are built with a soname (`-Wl,-soname,<name>.so`).
  Without one, the linker records a library by its path on the build
  machine.

### Kernel

- **Loading:** a Linux program may be `ET_EXEC`, or position-independent
  (`ET_DYN`), which is placed at 16 MiB. `abi/src/elf.rs` gained
  `parse_at`, which moves every address by a base, and reports
  `PT_INTERP`.
- **A program with `PT_INTERP`:**
  - Its loader is read from the initrd and checked as `ET_DYN`, which must
    not name a loader itself.
  - The loader is placed at 768 MiB, and the start-up vector gains
    `AT_BASE`.
  - Execution starts at the loader's entry. The program's own segments,
    `AT_PHDR` and `AT_ENTRY` are as before.
  - `execve` does the same.
- **`mmap`:**
  - It maps initrd files: a private copy of the requested pages, zero
    past the end of the file.
  - `MAP_FIXED` replaces what a reservation holds at that address; a free
    range becomes a new reservation. `MAP_FIXED_NOREPLACE` fails with
    `EEXIST` if anything is there.
  - Anonymous mappings stay lazy (L6).
- **`stat`** reports a device and an inode: initrd files use their
  address, home-folder files a hash of their path. musl's loader tells
  libraries apart by these, and with all zeros a second library would
  resolve to the first.
- Also: `pread64` on initrd files, and `membarrier`. With one CPU, every
  barrier is complete at once; without it, musl falls back to signalling
  every thread.

## 3. Verification and measurements

`linux-dovekie` requires the program's lines and its 20 children (10
static and 10 dynamic `hello` starts, each started, exec'd and exited
0). It also requires `libdovekie says: hello from libdovekie` and `all
checks passed`. The log shows no unsupported system calls.

**Measured** (WHPX, start + wait, averages of 10, three runs):

| | kcycles |
| --- | --- |
| static `hello` | 2134 / 2276 / 2323 |
| dynamic `hello-dyn` | 3307 / 2627 / 2824 |

A dynamic start costs about 0.4–1.1 Mcycles more. Most of that is copying
the loader's 865 KB of segments into each new process. Sharing the loader's
read-only pages between processes is a follow-up; the initrd's file data
is not page-aligned today.

Negative controls (each restored after):

| Change | Result |
| --- | --- |
| `AT_BASE` not given to the loader | `FAIL`: the loader crashes (page fault at 0x2f0e9f08) |
| Every file reports inode 0 | `FAIL`: exit 41, the plugin resolves to `libdovekie` |
| `MAP_FIXED` address ignored | `FAIL`: write fault, a segment landed elsewhere |

## 4. Not in step 8

- Mapping home-folder files or memfds; `MAP_SHARED` with writes visible
  to other mappings (shared mappings are private copies).
- Sharing read-only file pages between processes.
- glibc programs; running programs from the home folder; `$ORIGIN` and
  `/proc/self/exe`.
- `MAP_FIXED` over part of a reservation and part of free space.
- Wine itself: building it (its source and a PE cross-compiler) is the
  next step, and needs the owner's approval for the download.
