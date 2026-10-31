# POSIX step 5b-1: process arguments and the clock

Status: implemented 2026-10-31 (version 0.24.0, ABI version 16); §4
records the implementation. First part of POSIX step
5b (a C library port), which needs process arguments, time and file
descriptors. File descriptors are step 5b-2.

## 1. Process arguments

- A command line is the program name followed by its arguments, each
  ended by a NUL byte except the last: `tern-test\0alpha\0beta`.
- `spawn` and `spawn_with` accept it where they took the program name: the
  program is the part before the first NUL. A name without NUL has no
  arguments (every existing caller is unchanged). The whole command line
  may be up to 512 bytes; the program part keeps the existing path limit.
- The kernel keeps the command line with the process. New call 57
  `args_get(buf, len)` copies it into `buf` (up to `len` bytes) and
  returns its full length, so a caller can size its buffer. `argv[0]` is
  the program name.
- Shell: `run <program> [args...]` passes its words as the arguments.

## 2. Clock

- At boot the kernel reads the CMOS real-time clock (ports 0x70/0x71):
  waits (bounded) for no update in progress, reads until two reads agree,
  decodes BCD and 12-hour forms as status register B says, takes the year
  as 2000 + the two-digit year, and converts to Unix time (UTC; QEMU's RTC
  runs in UTC by default). It logs `ZOO_OS INFO clock unix=<seconds>`.
- New call 58 `clock(which)`: `0` wall-clock time, in nanoseconds since
  1970-01-01 UTC (boot RTC time plus time since boot); `1` monotonic time,
  in nanoseconds since boot. Resolution is the timer tick (10 ms); finer
  time can come with a calibrated TSC later.
- ABI version 16.

## 3. Verification

A new C test program, `tern-test` (Arctic tern), prints its arguments and
the wall-clock time, and checks that monotonic time increases across a
sleep. Case `args-clock`:

1. `run tern-test alpha beta` → `argc=3 argv=tern-test,alpha,beta`.
2. `unix=<seconds>` from the guest within 120 s of the host's clock at the
   same moment (new harness action that checks a number in the log
   against the host clock).
3. Monotonic time advanced by at least 0.5 s across a 50-tick sleep.

Negative controls: skip BCD decoding (the time is wrong by years: step 2
must fail); drop the arguments (step 1 must fail).

## 4. As implemented

- `kernel/src/rtc.rs`: as §2. Impossible dates (for example raw BCD read
  as binary) leave the clock unset: `ZOO_OS INFO clock none` and
  `clock(0)` returns `NOTFOUND`, rather than a wrong time.
- `spawn`/`spawn_with` take up to `MAX_CMDLINE` (512) bytes; the program
  name before the first NUL keeps `MAX_PATH`. The kernel shell's `run`
  joins its words with NUL.
- C: `zoo.h` gained `zoo_args_get`, `zoo_clock`, `zoo_sleep`;
  `zoo_abi.h` gained `ZOO_MAX_CMDLINE` and the call numbers.
- Harness action `WaitClock(text)`: the Unix seconds after `text` must be
  within 120 s of the host's clock.
- Negative controls: skipping BCD decoding made every date impossible, so
  it was caught by the program (exit code 12), not by the host
  comparison. A third control (the year off by one) shows that the host
  comparison rejects a plausible but wrong time.
