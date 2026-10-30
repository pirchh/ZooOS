# Firmware and emulator setup

OriginsOS boots under QEMU with OVMF (the EDK II UEFI firmware for virtual
machines). Firmware is never stored in Git (`*.fd` files here are ignored).

## Tested setup (Windows 11 host, 2026-10-30)

| Item | Value |
| --- | --- |
| QEMU | 11.1.0 (`v11.1.0-12130-ge470268ff4`), Windows build by Stefan Weil |
| Installed with | `winget install --id SoftwareFreedomConservancy.QEMU --exact` (installer `qemu-w64-setup-20260811.exe` from qemu.weilnetz.de; winget verified its hash) |
| Firmware source | EDK II images bundled with that QEMU build in `C:\Program Files\qemu\share\` |
| `OVMF_CODE` | `edk2-x86_64-code.fd`, 3,653,632 bytes, SHA-256 `33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a` |
| `OVMF_VARS_TEMPLATE` | `edk2-i386-vars.fd`, 540,672 bytes, SHA-256 `5d2ac383371b408398accee7ec27c8c09ea5b74a0de0ceea6513388b15be5d1e` |
| Firmware licence | BSD-2-Clause-Patent (EDK II); full texts in `share\edk2-licenses.txt` |

QEMU ships a single x86 variable-store template, named `edk2-i386-vars.fd`.
It is the companion of `edk2-x86_64-code.fd`; the variable-store format is
not architecture-specific. The `-secure-code` variant is not used (it needs
SMM and Secure Boot keys, which M1 does not require).

`cargo xtask doctor` prints the size and SHA-256 of whatever firmware it
resolves, so you can compare against the table above.

## How paths are resolved

1. Environment variables, if set, are used as-is and must name existing files:
   `QEMU_X86_64`, `OVMF_CODE`, `OVMF_VARS_TEMPLATE`.
2. Otherwise QEMU is looked up on `PATH`, then in `%ProgramFiles%\qemu`
   (Windows). Firmware is looked up next to it in `share\`, then in common
   Linux package locations (`/usr/share/qemu`, `/usr/share/OVMF`,
   `/usr/share/edk2/x64`, names `OVMF_CODE_4M.fd` / `OVMF_VARS_4M.fd`).
   The Linux fallbacks are untested.

The variable-store template is only ever read. Each QEMU run gets a private
copy in `build/runs/<case>/OVMF_VARS.fd`, so runs cannot affect each other or
the template. The code image is attached read-only.

## Setting up a new machine

Windows (needs an administrator prompt, which winget raises):

    winget install --id SoftwareFreedomConservancy.QEMU --exact

Debian/Ubuntu (untested here): `sudo apt install qemu-system-x86 ovmf`, then
set `OVMF_CODE=/usr/share/OVMF/OVMF_CODE_4M.fd` and
`OVMF_VARS_TEMPLATE=/usr/share/OVMF/OVMF_VARS_4M.fd` if doctor does not find them.

Then run `cargo xtask doctor` and check that it ends with
`ready: all prerequisites found`.
