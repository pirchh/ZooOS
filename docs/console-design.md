# Interim console design: screen text, keyboard, debug shell

Status: designed first, then implemented and verified (2026-10-30); see
docs/verification.md. This is an
**interim step pulled forward from M4** at the user's request, so the system
can be seen and typed into before M3. It is a kernel-mode debug console, not
the future desktop, terminal application or user-space shell; those remain
M4 and later. Results go in docs/verification.md.

## Goal

Boot in a QEMU window, see kernel output on the screen, type with the
keyboard, and run a handful of built-in diagnostic commands. Everything the
screen shows is also sent to COM1, so the automated verifier keeps working.

## 1. Framebuffer text console

- Source: the UEFI GOP framebuffer the bootloader passes in
  `BootInfo.framebuffer` (QEMU default: 1280×800, 4 bytes per pixel, BGR).
  The kernel takes it out of `BootInfo` once and owns it from then on.
- Font: `noto-sans-mono-bitmap` 0.3.2, regular weight, 16 px raster height,
  Basic Latin only. Characters outside the font are drawn as `?`.
  Licence: crate MIT; glyph data from Noto Sans Mono (SIL OFL).
- Layout (shared with the verifier, which re-renders text to check pixels):
  8 px margin on all sides, cell = font raster width × 18 px line height
  (16 px glyph + 2 px gap). Columns and rows are derived from the resolution.
- Colours: black background; glyph pixel = glyph intensity `i` written as
  `(i, i, i)`. No other colours in this step, so a screenshot can be checked
  pixel for pixel.
- Supported pixel formats: `Rgb`, `Bgr`, `U8` (greyscale). `Unknown` formats
  disable the screen console (serial still works) with an `INFO` line.
- Scrolling: when the cursor passes the last row, all rows move up one line
  by copying framebuffer memory, and the last row is cleared. Simple and
  correct; fast enough in QEMU. *Deferred:* a back buffer (needed on real
  hardware, where reading video memory is slow).
- `kprintln!` writes each line to COM1 and to the screen. The old
  `serial_println!` stays for code that must not touch the console lock.
- Locking: the console sits in a `spin::Mutex`. Normal code locks it. Panic
  and fatal-exception handlers use `try_lock`: if the lock is held (a fault
  while drawing), they skip the screen and still report on serial, so a
  fault can never deadlock the report.

## 2. PS/2 keyboard

- QEMU's q35 machine has an i8042 controller with a PS/2 keyboard, using
  scancode set 1 (translated) as OVMF leaves it.
- IRQ1 is unmasked at the PIC (alongside IRQ0, the timer). Its handler reads
  one byte from port `0x60`, pushes it into a queue and sends end-of-interrupt.
  **The handler never decodes, allocates, prints or locks the console.**
- Queue: a fixed 256-byte single-producer/single-consumer ring of atomics.
  The IRQ handler is the only producer, the shell loop the only consumer.
  When full, new bytes are dropped and counted.
- Decoding happens in the shell loop with `pc-keyboard` 0.9.0
  (`ScancodeSet1`, US 104-key layout, Ctrl not mapped).
- Before unmasking IRQ1, bytes already waiting in the controller are drained
  with a bounded loop.
- *Deferred:* full i8042 initialisation and self-test, other layouts, USB
  keyboards (needed on most real PCs), key repeat settings, mouse.

## 3. Debug shell (in-kernel)

- Starts after `READY`: prints `ZOO_OS SHELL ready`, then the prompt
  `zoo> `.
- Loop: with interrupts disabled, check the queue; if empty, `sti; hlt`
  atomically (so a key arriving between the check and the halt still wakes
  it). Otherwise decode and handle the key.
- Line editing: printable characters are appended and echoed; Backspace
  deletes one character; Enter submits. The line is at most 120 characters.
- On Enter the kernel prints `ZOO_OS SHELL line=<text>` to serial only
  (the screen shows the typed line already), then runs the command.
- Commands:

  | Command | Does |
  | --- | --- |
  | `help` | lists commands |
  | `echo <text>` | prints the text |
  | `mem` | free/total physical frames, heap bytes in use |
  | `uptime` | timer ticks and seconds since the timer started |
  | `keys` | keyboard bytes received and dropped |
  | `clear` | clears the screen |
  | `panic` | deliberate panic (tests the panic path from the shell) |
  | `reboot` | pulses the CPU reset line via the i8042; QEMU with `-no-reboot` exits |

  Anything else prints `unknown command: <name>`.
- The shell owns the `Memory` value, so `mem` reads real allocator state.

## 4. Markers and verification

Normal boots now end with `ZOO_OS READY` followed by
`ZOO_OS SHELL ready`. A new verifier case drives the real guest:

1. Start QEMU with a QMP control socket on `127.0.0.1` (free port chosen at
   run time) in addition to the existing serial log.
2. After `SHELL ready`, send keystrokes with QMP `send-key`. They go through
   the emulated PS/2 keyboard, IRQ1, the queue, the decoder and the shell:
   `e c h x Backspace o Space h i Enter`, then `m e m Enter`.
3. Expect `ZOO_OS SHELL line=echo hi` (proves Backspace works) and
   `ZOO_OS SHELL line=mem` on serial.
4. Take a QMP `screendump` (PPM, plus a PNG copy for people) and check it:
   - row 0 matches `ZOO_OS BOOT version=<version>` pixel for pixel,
     re-rendered on the host with the same font and layout;
   - some row matches `zoo> echo hi` pixel for pixel;
   - a deliberately wrong string (`ZOO_OS BOOT version=9.9.9`) does
     **not** match row 0, proving the comparison can fail.

Added during implementation: a `scroll` case types `help` four times and
`echo end`, which overflows the 43-row screen, then checks that the last
three rows are exactly `zoo> echo end`, `end` and the prompt, and that
the BOOT line has scrolled off row 0.

Fault and panic kernels draw their reports on screen as well; their
verifier cases are unchanged apart from the extra `SHELL ready` in normal
mode. `cargo xtask run` now opens a QEMU window (keyboard goes there) and
still shows COM1 in the terminal.

## 5. Out of scope

Desktop, windows, mouse, colours and themes, Unicode beyond Basic Latin,
user-space programs, a real terminal, persistent history, USB input and
real-hardware testing.
