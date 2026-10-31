# C toolchain (Zig)

The toolchain is downloaded, never committed (the archive and the extracted
folder are ignored by Git). The owner approved this download on 2026-10-30.

| Field | Value |
| --- | --- |
| Toolchain | Zig 0.16.0 (released 2026-04-13), used as `zig cc` (clang 21.1.0, lld) |
| File | `zig-x86_64-windows-0.16.0.zip`, 97,217,739 bytes |
| Source | https://ziglang.org/download/0.16.0/zig-x86_64-windows-0.16.0.zip (listed in https://ziglang.org/download/index.json) |
| SHA-256 | `68659eb5f1e4eb1437a722f1dd889c5a322c9954607f5edcf337bc3684a75a7e` (matches the index) |
| Licence | MIT (Zig); bundled clang/LLVM under Apache-2.0 with LLVM exceptions |

Why Zig: one pinned archive provides a C compiler, a linker and libc
sources (musl) for the later libc port, with nothing installed system-wide.
0.16.0 was chosen over 0.17.0 (2026-10-01) because it had been released
for six months.

To fetch it on a new machine (Windows host; about 97 MB):

    curl -fL -o tools/zig/zig-x86_64-windows-0.16.0.zip \
      https://ziglang.org/download/0.16.0/zig-x86_64-windows-0.16.0.zip
    sha256sum tools/zig/zig-x86_64-windows-0.16.0.zip   # must print the value above
    cd tools/zig && unzip zig-x86_64-windows-0.16.0.zip

`cargo xtask build`/`verify` use `tools/zig/zig-x86_64-windows-0.16.0/zig.exe`
and refuse a different `zig version`. Without the toolchain the C programs
are left out of the image and the `c-program` verify case reports `SKIP`.
