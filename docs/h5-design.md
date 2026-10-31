# Hardware step 5 (H5): key repeat for USB keyboards

Status: implemented 2026-10-31 (version 0.35.0). Follows H1b. A PS/2
keyboard repeats a held key by itself; a USB keyboard in boot protocol
(with SET_IDLE 0) reports only changes, so the host must repeat. Without
it, holding a key on a USB keyboard typed one character.

## 1. Design

- The key pressed last repeats while it is held: after 500 ms (50 ticks),
  then every 30 ms (3 ticks, about 33 a second), like PS/2 defaults. The
  repeat is another set-1 make code in the keyboard queue, which is what a
  PS/2 keyboard sends. Releasing that key, or pressing another, ends or
  moves the repeat; modifiers never repeat.
- Done in the xHCI timer-tick poll, after the transfer events.

## 2. Verification

- Harness action `HoldKey(qcode, ms)` (QMP `input-send-event` key down,
  wait, key up) and marker prefixes (`then_prefix`) for lines whose end
  varies.
- `usb-repeat` case: hold `x` for 1.5 s, wait for the log to go quiet,
  press Enter: `SHELL line=` followed by 34 x's (at least ten required).
  The quiet wait only ends if the repeat stopped on release.

Negative controls:

| Change | Result |
| --- | --- |
| No repeat | `FAIL usb-repeat`: `got ZOO_OS SHELL line=x` |
| Repeat not stopped on release | `FAIL usb-repeat`: Enter, pressed last, kept repeating (`got ZOO_OS SHELL line= after the final expected marker`) |

## 3. Not done

- Fixed delay and rate (no settings yet).
- PS/2 keyboards keep their own repeat; QEMU's QMP key presses do not
  exercise it.
