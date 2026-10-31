# Desktop step 6 (D6): start in the desktop

Status: implemented 2026-10-31 (version 0.28.0), as proposed. Follows B1
(fast boot).

## 1. Change

- The fast-boot build, which `cargo xtask run` boots, starts the desktop
  session as soon as the shell is ready, as if `desktop` had been typed;
  the serial log says `ZOO_OS SHELL autostart desktop`. Escape still ends
  the session and leaves the debug shell, where `desktop` starts it again.
- The normal build (every test case) is unchanged: it stays in the shell
  until told otherwise.

## 2. Verification

The `fast-boot` case becomes: boot → the desktop starts by itself (its
processes and the display) → screenshot: the desktop with `Files`
focused → Escape → the shell runs `run tern-test fast` as before.

Negative control: no autostart; the case must fail (the desktop never
starts).

## 3. As implemented

`kernel/src/shell.rs` runs the `desktop` command after `SHELL ready` when
built with `fast-boot`. In the `fast-boot` case the session's processes
are pids 5–9 and TernTest pid 10. Negative control result: `FAIL
fast-boot` (timeout: `no ZOO_OS SHELL autostart desktop within 30s`).
