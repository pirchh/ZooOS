# UI step 1b (U1b): input without waiting for the tick

Status: implemented 2026-11-01 (version 0.54.1). After trying U1, the
owner said the desktop felt jittery and rigid.

## 1. The cause

The window server (PeacockHandler) looked for input once per loop and,
when nothing had changed, slept one timer tick (`sleep(1)`, 10 ms at
100 Hz). Two things then held a mouse event back:

- A sleeping task wakes only on the timer tick. A mouse interrupt queued
  the event but woke nobody, even when the processor was idle.
- Only the timer could switch away from the idle kernel task. The keyboard
  and mouse interrupts were plain handlers.
- Even a ready program could wait: when a tick gave the kernel task a turn
  (it had seen input), the kernel task halted again until the next tick
  with the program still ready.

So every event waited from its arrival to the next tick: anywhere from 0
to 10 ms, about 5 ms on average. The pointer moved on the tick grid, not
when the mouse moved. That grid beats against the host display's 60 Hz,
so equal hand movements came out as uneven steps.

Measured with a new counter (§3) on the partial-redraw sweep of
`--accel whpx --case frames` (40 events per sweep, three runs each):

| Build | Mean wait | Longest wait |
| --- | --- | --- |
| Before (polling each tick) | 14,399,606–16,097,558 cycles | 30,875,290–32,043,520 cycles |
| Input wakes the window server, kernel task still halts when a program is ready | 60,848–839,125 cycles | 145,766–31,946,041 cycles |
| As merged | 52,523–89,478 cycles | 113,111–533,743 cycles |

A tick is about 31 M cycles here, and drawing a pointer frame costs about
60,000. Before, an event waited about 270 times longer than its frame took
to draw. Now it waits about as long as one frame.

## 2. The change

- **Input interrupts can switch tasks.** IRQ1 (keyboard) and IRQ12
  (mouse) now enter through the same assembly path as the timer
  (`trap.rs`, `VEC_KEYBOARD`, `VEC_MOUSE`).
  - After queueing the input, `process::on_input` readies any task
    blocked in `INPUT_WAIT`.
  - If the kernel task was halted in `idle_hlt` (its only preemption
    point), it switches to that task at once.
  - `idle_hlt` no longer halts while a program is ready. It gives up the
    processor through a ring-0-only gate (`int 0x81`, `VEC_KERNEL_YIELD`)
    instead.
  - If another program is running, nothing is preempted: the woken task
    runs at that program's next tick, as before.
- **A new system call, `INPUT_WAIT` (64)**, for the display owner only.
  - It blocks until keyboard or mouse input is queued, any other wake-up
    (a message or an exit, which already wake blocked tasks), or a
    deadline in ticks.
  - It returns at once if input is already waiting. Interrupts are off
    from that check until the switch, so input cannot slip in between.
- **USB devices.** USB keyboards and mice are still polled on the tick
  (docs/h1b-design.md). The tick now wakes input waiters for what it
  polled, so they react on that tick rather than the one after.
- **The window server** waits with `INPUT_WAIT` until the next second
  (for the clock), instead of `sleep(1)`.

Nothing else in the drawing changed.

## 3. Measuring it

- **The counter:** the kernel stamps each mouse event with the time-stamp
  counter when it is queued. `MOUSE_READ` adds the time from that stamp to
  the read to a count, a sum and a maximum.
- **Reading it:** `INPUT_LATENCY` (65) returns those figures and restarts
  them. PeacockHandler logs them with its frame report (F11), in cycles
  and in thousandths of a tick. The window server measures the
  counter's rate itself, from the time since it started.
- **The check:** the `frames` case now requires the partial-redraw sweep
  to have at least 20 events, a mean wait of at most 250 thousandths of a
  tick (2.5 ms), and a longest wait of at most 500. Waiting for the tick
  averages about 500 with a longest of about 1000.

## 4. Not done

- **Absolute pointer:** under QEMU the pointer is a relative PS/2 mouse,
  so the QEMU window grabs the host pointer. A USB tablet would let the
  ZooOS pointer follow the host pointer exactly, without the grab. Not
  done; it needs an absolute event in the mouse ABI and a HID report
  parser.
- **USB input:** still polled at 100 Hz; interrupt-driven xHCI events
  would remove that.
- **Window drags:** a large window still repaints the whole screen each
  frame (about 8.5 M cycles, roughly 3 ms with WHPX), because its old
  and new areas together exceed half the screen.
