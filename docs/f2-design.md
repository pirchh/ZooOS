# Fast step 2 (F2): kernel output and memory reuse off the hot path

Status: implemented 2026-10-31 (version 0.45.0). Follows L5
(docs/linux5-design.md), whose fork timings showed where the time went.

## 1. What the measurements showed

L5's `guillemot-test` measured `fork` + `exit` + `wait` at 9.4–12.5
million cycles under WHPX. Probes inside the kernel split that up:

- **One kernel log line cost 2.2–4.3 Mcycles.** The serial driver polled
  the line-status register before every byte, so each byte took two port
  accesses, and each port access is a slow exit to the emulator. The text
  console then copied the whole framebuffer up one line, about 4 MB, for
  every new line. A fork prints two lines.
- **Copying a child's 132 pages cost about 3 Mcycles, of which the copy
  itself was 0.1.** The rest was the first write to frames the guest had
  never used: under a hypervisor each costs about 23,000 cycles while the
  host backs it. The allocator handed out new frames each time, because
  ended programs were only freed on the next 10 ms tick and its hint moved
  on through memory.

## 2. Changes

- **Buffered serial output** (`kernel/src/serial.rs`):
  - Text goes into a 64 KiB ring. Whenever the UART reports room, the
    driver writes 16 bytes (its FIFO) with one `rep outsb`, and it never
    waits.
  - What is left goes out on the next write, the timer tick or when idle.
    A full ring makes room the old, slow way, so nothing is dropped.
  - `flush` (in `halt_forever`, so on every panic and fatal fault, and
    before the deliberate triple fault) sends everything and switches to
    direct output.
  - At boot the driver checks the UART's scratch register. A PC without a
    COM port, as most laptops are, has serial output turned off: no port
    accesses at all.
- **Batched console drawing** (`kernel/src/console.rs`):
  - The console keeps its text in a grid (at most 256 × 128 cells).
  - Characters are drawn as they come, as before, until a line scrolls.
    The grid then moves instead of the framebuffer, and the screen is
    redrawn once, at the next tick, idle moment or halt.
  - The redraw only writes video memory, never reads it, which also
    matters on real graphics hardware.
  - Pixels end up identical: the `scroll` case compares them exactly.
- **Warm frame reuse** (`kernel/src/memory/frames.rs`,
  `kernel/src/process.rs`):
  - The allocator keeps a stack of the last 4096 frames freed and hands
    those out first, newest first. They are still in the CPU's caches, and
    under a hypervisor already backed.
  - Ended programs are reaped at every task switch, not only on ticks, so
    their frames are back before the next program starts.

## 3. Results

WHPX, `guillemot-test`, averages per child, kcycles. The fourth column is
three runs per stage, and nine runs for the finished code:

| | L5 (before) | Serial buffered | + console batched | + frame reuse (finished) |
| --- | --- | --- | --- | --- |
| fork + exit + wait | 9430–12476 | 5720–6373 | 5303–5351 | 1535–2530 |
| posix_spawn + wait | 8201–8746 | 4234–5052 | 3234–3529 | 1978–3156 |

With a 4 MiB buffer that the children really read: 2268–5402 kcycles on
the finished code. L5 reported this as costing the same as a plain fork.
That was wrong: the compiler had removed the unread buffer, and the test
now forces it to be real. Copying 4 MiB adds roughly 1–3 Mcycles, so
copy-on-write `fork` stays deferred.

Kernel's first line → `SHELL ready` (`fast-boot`, WHPX): before 174 /
166 / 167 ms; finished 111 / 110 / 178 ms. Most runs after the change
were 109–112 ms, with occasional runs near 170. The harness reads serial
output every 50 ms, so this is coarse.

The WHPX runs share the host with other work (a QEMU desktop session was
running throughout), so ranges are wide. Real hardware has no emulator
exits, but polled serial at 38400 baud took about 12 ms per line there,
and a missing COM port is now skipped entirely.

## 4. Negative controls

| Change | Result |
| --- | --- |
| The console is never redrawn after scrolling (tick and idle) | `FAIL scroll`: row 40 does not show `zoo> echo end` |
| Buffered serial output never sent | `FAIL normal` (timeout): no `ZOO_OS BOOT` within 30 s |
| A reused frame is not marked taken | `FAIL normal`: `PANIC self-test frames: free count not restored` |

## 5. Not in this step

- Interrupt-driven serial output, which would keep a real UART busy
  without polling at ticks.
- Copy-on-write `fork`.
- Quieter process logging in release images. `ZOO_OS PROC` lines are the
  test protocol and are kept.
