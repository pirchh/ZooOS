# Hardware step 1b (H1b): USB keyboard and mouse input

Status: implemented 2026-10-31 (version 0.31.0). Follows H1a
(docs/h1-design.md): the xHCI driver now takes input from HID keyboards and
mice, so ZooOS can be used on a PC without PS/2.

## 1. Design

- **Setting a device up** (during enumeration, for the first HID boot
  interface with an interrupt IN endpoint): SET_CONFIGURATION,
  SET_PROTOCOL(boot), SET_IDLE(0) (reports only on change; a device may
  refuse it), then a Configure Endpoint command adding the endpoint
  (interrupt IN, its max packet size, the interval converted to xHCI's
  2^n × 125 µs form, its own transfer ring). Logs `usb port=N input
  ready` or `input error=…`.
- **Polling:** after enumeration the controller moves into a lock
  (`HC`); every timer tick (100 Hz) handles the events that have
  arrived: each finished report is processed and the next transfer
  queued (one outstanding per endpoint). Logs `usb keyboard input` /
  `usb mouse input` at the first report of each.
- **Keyboard:** boot reports (modifiers, six key usages) are compared with
  the previous report; presses and releases become scancode set 1 bytes
  (with `0xE0` for extended keys: arrows, right Ctrl/Alt, GUI) in the same
  queue the PS/2 keyboard fills. So the shell, the desktop and every
  application work unchanged. A report with the rollover error code is
  ignored. Usages covered: letters, digits, Enter, Escape, Backspace, Tab,
  Space, punctuation, Caps Lock, F1–F12, arrows, all eight modifiers.
- **Mouse:** boot reports (buttons, dx, dy) go into the PS/2 mouse's event
  queue, with y negated (USB counts downwards, the queue upwards).
- The keyboard and mouse queues gained a `push` for this second producer.
  Both producers run in interrupt handlers, which do not nest, so each
  queue still has one producer at a time.

## 2. Verification

- `usb` case: both devices `input ready`; `echo usb` typed (QEMU sends the
  keys to the USB keyboard once it is configured) → `usb keyboard input`
  and `SHELL line=echo usb`.
- `usb-desktop` case: `desktop` typed through the USB keyboard; a USB
  mouse move and click on Crash Test → `usb mouse input`, `click focuses
  Crash Test`, screenshot check as in the `mouse` case.

Negative controls:

| Change | Result |
| --- | --- |
| USB key presses not queued | `FAIL usb`: `SHELL line=echo usb` never came. This also shows the keys did not reach the PS/2 keyboard. |
| USB mouse y not negated | `FAIL usb-desktop`: the click landed on the Assistant (`click focuses Assistant`) |

## 3. Not done

- No key repeat (the desktop and shell never had it), no keyboard LEDs,
  no layouts other than US, no scroll wheel.
- Only devices on root ports (no hubs); one HID interface per device.
- Polled at 100 Hz (up to 10 ms extra latency); interrupts come later.
- Real hardware untested (QEMU only).
