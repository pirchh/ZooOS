# Desktop step 1 (D1): a mouse

Status: implemented 2026-10-31 (version 0.21.0, ABI version 15); §4 records
where the implementation differs from the proposal. First desktop step after
the services milestone, towards the owner's goal of a macOS-like feel.

## 1. Input

- **Kernel:** enable the i8042 auxiliary (mouse) port, turn on data
  reporting, unmask IRQ12. Standard 3-byte PS/2 packets (buttons, dx, dy
  with sign bits), resynchronised on bit 3 of the first byte.
- Mouse packets join the keyboard bytes in the input queue the display
  owner reads, as a tagged 4-byte event (`0xF0` marker, buttons, dx, dy),
  so `input_read` keeps one stream and its ordering. The kernel shell
  ignores mouse events.
- QEMU's PS/2 mouse is relative; absolute pointing (USB tablet) can come
  later.

## 2. Window server

- PeacockHandler keeps a pointer position, clamped to the screen, and draws
  a small arrow on top of everything.
- **Click** in a window: that window takes focus. **Drag** on a title bar:
  the window moves (follows the pointer while the button is held). Window
  positions become state instead of the fixed three-column layout; the
  starting layout stays the same, so the existing screen checks still
  apply.
- Keyboard focus follows clicks; Tab still cycles.

## 3. Verification

A `mouse` case (QMP `input-send-event`, relative moves and button
presses):

1. Desktop starts; the pointer is drawn at its start position (screenshot).
2. Move the pointer over the Crash Test window and click: Crash Test is
   focused (title colour check), without any key press.
3. Drag the Assistant window's title bar by a known amount: the Assistant
   title is found at the new position, and no longer at the old one.

Negative control: ignore the mouse packet's sign bits (movement goes the
wrong way); the click and drag checks must fail.

## 4. As implemented

- **Separate call instead of a shared input stream.** Mouse events come
  from call 56 `mouse_read` (8-byte `MouseEvent`: dx, dy, buttons), not
  tagged into `input_read`. Keyboard bytes stay raw scancodes for the
  `pc_keyboard` decoder, which would otherwise have to skip tagged events;
  the kernel shell never sees mouse events. Same rule as the keyboard:
  only the display owner may read them. The queue is cleared when the
  display changes owner.
- **Kernel** (`kernel/src/mouse.rs`): i8042 auxiliary port enabled at boot
  with bounded waits (`ZOO_OS INFO mouse ps2=true`); if any step fails the
  mouse stays off and IRQ12 stays masked. IRQ12 and IRQ2 (the cascade) are
  unmasked. Both IRQ1 and IRQ12 check the controller's AUX status bit, so
  a mouse byte read by the keyboard handler still reaches the mouse.
  Packets go into a 128-entry lock-free ring; overflowing packets are
  dropped.
- **Window server:** window rectangles and a back-to-front order; the
  focused window is drawn last. Windows are kept on screen while dragged
  (title bar reachable). Logs `click focuses <window>` and
  `moved <window> to (x, y)`.
- **Harness:** QMP `input-send-event` (relative moves in steps of at most
  100 pixels, left button), actions `Mouse(dx, dy)` and `Button(down)`;
  case `mouse`.
