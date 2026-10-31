# Boot step 1 (B1): fast boot without the check programs

Status: implemented 2026-10-31 (version 0.26.0). Towards the owner's goal of
a fast, lean system. Measurement came first; the change follows from it.

## 1. Measurement

The harness now records when each serial line first appears, in
milliseconds since QEMU started (`build/runs/<case>/serial-times.log`,
polled every 50 ms). A normal boot under QEMU/WHPX on the development PC
(`cargo xtask verify --skip-model --accel whpx --case normal`):

| Phase | Time |
| --- | --- |
| QEMU start → first kernel line (UEFI firmware, bootloader) | about 1.0 s |
| Kernel self-tests (of which the timer test waits 10 ticks, 0.1 s) | about 0.1 s |
| Boot-check programs: Zookeeper in check mode runs 20 test processes | about 0.96 s |
| of which `caribou-test` (threads/futex tests that sleep on purpose) | about 0.55 s |
| Resident supervisor and services → `SHELL ready` | under the 50 ms poll |

So about 90% of the kernel's part of boot was tests that every boot ran.

## 2. Change

- Kernel feature `fast-boot`: after the self-tests, skip the check
  programs and start the resident supervisor directly. Logs `ZOO_OS INFO
  boot checks skipped (fast-boot)`.
- Image mode `fast-boot`. `cargo xtask run` boots it by default; `--mode
  normal` gives the full checks. `verify` builds both: every existing case
  keeps the normal build (and its process numbers), and a new `fast-boot`
  case checks that the supervisor, its services, the shell and a C program
  work without the checks.
- Mode features are counted (at most one) instead of listed in pairs.

## 3. Results

Three runs each (`--accel whpx`, `--case normal` / `--case fast-boot`):

| Build | Kernel's first line → `SHELL ready` | QEMU start → `SHELL ready` |
| --- | --- | --- |
| normal | 1,072 / 1,058 / 1,075 ms | 2,090 / 1,973 / 2,099 ms |
| fast-boot | 108 / 108 / 110 ms | 1,136 / 1,284 / 1,238 ms |

The kernel's part of boot is about 10 times shorter. The remaining second
is UEFI firmware in QEMU, which real hardware replaces with its own
firmware. Times are at the harness's 50 ms polling resolution.

Negative control: `fast-boot` with the check still running (the condition
forced false): `FAIL fast-boot`, `got ZOO_OS PROC start pid=2
name=beluga-test while expecting ... badger-handler`.

## 4. Not done

- The timer self-test's 0.1 s wait (an M2 check) remains in both builds.
- Firmware time is not addressed (QEMU's OVMF; it also tries the data disk
  before the boot disk).
- No real-hardware boot time yet.
