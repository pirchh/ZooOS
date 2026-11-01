# UI step 1c (U1c): USB input by interrupt, and a tablet for QEMU

Status: implemented 2026-11-01 (version 0.55.0). U1b made input wake the
window server at once. The owner still felt a slight lag and asked for an
engineer's call on what else stands between the hand and the pointer.

## 1. Where the time went

- **Inside ZooOS (measured in U1b):** an event reaches the window server
  in about 50,000 cycles, and a pointer frame costs about 60,000. Nothing
  more worth taking there.
- **USB input (measured here):** reports were collected on the 10 ms
  tick, with no interrupts. Every modern PC's mouse is USB, so on real
  hardware this was the same tick wait U1b removed for PS/2.
- **QEMU's mouse (not measured; QEMU's side):** the relative PS/2 mouse
  makes QEMU grab the host pointer. QEMU then converts host motion into
  PS/2 packets. The pointer's speed curve is no longer Windows'.
- **QEMU's display (not measured; QEMU's side):** the guest draws its
  pointer into an emulated framebuffer, and QEMU copies that to its window
  on its own refresh timer. On Windows and macOS the GPU moves a hardware
  cursor plane without redrawing anything. Matching that needs a GPU
  driver with a cursor plane (virtio-gpu in QEMU; a real driver on
  hardware), which is beyond this step.

## 2. USB input by interrupt

- **Interrupt delivery:** the xHCI driver points the controller's
  interrupt at the local APIC (vector 0x50, `VEC_XHCI`):
  - through MSI-X (entry 0 of its table, which must lie in the mapped
    BAR 0), or else MSI;
  - interrupter 0 runs without moderation, and `USBCMD.INTE` is set.
- **QEMU:** `qemu-xhci` offers only MSI-X, so both were needed. Real
  controllers usually offer both.
- **The interrupt path:**
  - acknowledge (`USBSTS.EINT`, `IMAN.IP`), then take the reports;
  - then the same `process::on_input` as the PS/2 interrupts.
- **The tick still polls as a fallback:** without a local APIC or either
  capability, or when the interrupt found the controller busy.
- **Logging:** `ZOO_OS INFO usb interrupts=msix|msi|polled`.
- **Timestamps:** a USB mouse event is stamped with the time its
  interrupt arrived, so the U1b wait counter covers the time to the tick.

Measured on the 40-event sweep of `--accel whpx --case usb-desktop`:

| Build | Mean wait | Longest wait |
| --- | --- | --- |
| Interrupt only stamps; reports taken at the tick (the old timing) | 17,009,645–19,916,866 cycles | 31,775,496–146,450,574 cycles |
| Reports taken in the interrupt | 386,161–520,623 cycles | 940,619–2,876,353 cycles |

The tablet case's three runs are included in the second row.

## 3. A tablet for `cargo xtask run`

- **The device:** `run` now gives QEMU a USB tablet (`usb-tablet`) instead
  of relying on the PS/2 mouse. QEMU then sends absolute positions and
  does not grab the host pointer. The ZooOS pointer goes where the
  Windows pointer is, with Windows' own speed curve.
- **The parser:** the tablet has no boot protocol, so its report
  descriptor is read (`GET_DESCRIPTOR` 0x22) and parsed (`kernel/src/
  hid.rs`) for X, Y and buttons 1–3:
  - report IDs, logical ranges, signed fields and usage ranges are
    handled;
  - push/pop and long items are skipped.
- **Which interfaces:** a HID interface of subclass 0 is tried as such a
  pointer. One without X and Y is refused (`not-a-pointer`).
- **The ABI:** `MOUSE_ABSOLUTE` (0x80) in `MouseEvent::buttons` marks a
  position, with `dx`/`dy` from 0 to `MOUSE_ABS_MAX` (0x7FFF), y
  downwards. The window server rounds it to a pixel.
- **The `tablet` case:**
  - it moves straight to Crash Test's close light (12 px), clicks, and
    expects Crash Test to close;
  - then it runs the pointer sweep with absolute moves and the U1b wait
    check.

The verify cases keep their PS/2 and USB mice. The relative path stays
covered, and on real hardware mice are relative.

## 4. Not done

- **Pointer acceleration** for relative mice on real hardware. A good
  curve is tuned by feel on a real mouse, and the owner's other PCs have
  not booted ZooOS yet. Under QEMU the tablet already carries Windows'
  curve.
- **A hardware cursor:** see §1. This is the remaining display-side lag
  in QEMU.
- **Mouse wheels and more than three buttons:** not parsed.
- **Boot-protocol devices stay in boot protocol:** high-resolution mice
  report at most ±127 per report there.
