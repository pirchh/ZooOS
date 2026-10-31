# musl source (for the dynamic loader)

The source is downloaded, never committed (the archive and the extracted
folder are ignored by Git). The owner approved this download on 2026-10-31.

| Field | Value |
| --- | --- |
| Library | musl 1.2.5 (released 2024-02-29) |
| File | `musl-1.2.5.tar.gz`, 1,080,786 bytes, and its signature `musl-1.2.5.tar.gz.asc` |
| Source | https://musl.libc.org/releases/musl-1.2.5.tar.gz (and `.asc`) |
| SHA-256 | `a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4` |
| SHA-512 | `7bb7f7833923cd69c7a1a9b8a5f1784bfd5289663eb6061dcd43d583e45987df8a68a1be05d75cc1c88a3f5b610653d1a70f4a9cff4d8f7fd41ae73ee058c17c` (equals the value in Alpine Linux's `aports` `main/musl/APKBUILD`, 3.21-stable) |
| Signature | Good signature from "musl libc <musl@libc.org>", key `8364 8929 0BB6 B70F 99FF DA05 56BC DB59 3020 450F` (key from https://musl.libc.org/musl.pub, checked in a throwaway GnuPG home) |
| Licence | MIT |

Why: Zig's bundled musl omits the dynamic linker (`ldso/dynlink.c`), so it
cannot provide `/lib/ld-musl-x86_64.so.1`. `cargo xtask build` compiles
this source with the pinned Zig into `libc.so` (xtask/src/musl.rs,
docs/linux8-design.md), caches it in `build/musl-1.2.5/`, and packs it as
`lib/ld-musl-x86_64.so.1`.

To fetch it on a new machine (about 1 MB):

    curl -fL -o tools/musl/musl-1.2.5.tar.gz https://musl.libc.org/releases/musl-1.2.5.tar.gz
    sha256sum tools/musl/musl-1.2.5.tar.gz   # must print the value above
    cd tools/musl && tar xzf musl-1.2.5.tar.gz

Without it, the build leaves out the dynamic loader, the shared libraries
and the dynamically linked programs, and says so.
