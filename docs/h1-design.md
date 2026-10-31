# Hardware step 1a (H1a): a USB host controller (xHCI), enumeration

Status: implemented 2026-10-31 (version 0.30.0). First step towards booting
ZooOS on the owner's other PCs from a USB stick (never on the development
PC): modern PCs have USB keyboards and mice, and ZooOS only spoke PS/2.
This step finds and starts the controller and identifies the devices; HID
keyboard and mouse input is step H1b.

## 1. Design

- `kernel/src/xhci.rs`, polled, run once at boot after the data disk.
- **Finding it:** PCI scan of bus 0 and every bus behind a PCI-to-PCI
  bridge (not all 256 buses: each configuration read is a port access,
  slow under virtualisation), for class 0C/03/30. The register BAR's size
  is probed; the registers are mapped uncached (`Memory::map_mmio`, a new
  kernel region at `0xFFFF_C000_C000_0000`). Memory space and bus
  mastering are enabled.
- **Taking it over:** the USB legacy-support capability (firmware
  hand-off, bounded wait), then halt, reset, wait for controller-not-ready
  to clear.
- **Data structures** (one zeroed 4 KiB page each, from the frame
  allocator): device context base array (with scratchpad buffers when the
  controller asks), a command ring and one transfer ring per device (255
  TRBs and a link TRB with toggle-cycle), and one event-ring segment with
  its one-entry segment table on interrupter 0.
- **Per connected root port:** reset it if it is not already enabled (USB
  2 ports), Enable Slot, Address Device (input context: slot context with
  speed and root port, endpoint 0 with the default packet size for the
  speed), then GET_DESCRIPTOR for the device and configuration
  descriptors. A transfer counts as done when the status stage's event
  arrives. Logs, for each device, `ZOO_OS INFO usb port=N
  vendor=0x… product=0x… class=C`, and `hid=keyboard`/`hid=mouse` for HID
  boot interfaces.
- Every wait is bounded (`SPINS`); a failing step is logged as `usb
  port=N error=…` and the boot continues.

## 2. Verification

- Cases have a `usb` flag; only the `usb` case attaches `qemu-xhci` with a
  `usb-kbd` and a `usb-mouse`. Elsewhere they would take keyboard input
  away from the PS/2 keyboard every other case types through.
- `usb` case: `usb xhci ports=8 slots=32 context=32`, `hid=keyboard` (port
  5), `hid=mouse` (port 6), both with QEMU's vendor id `0x0627`, `usb
  devices=2`, then the normal boot to `SHELL ready`.
- Boot time (fast-boot, WHPX, kernel's first line → `SHELL ready`, seven
  runs): 110, 168, 160, 110, 117, 109, 112 ms, against 108–110 ms before
  (B1); no measurable change at the harness's 50 ms polling resolution
  except two runs one poll later. A first version scanned all 256 buses
  and cost about 110 ms; it was replaced by the bridge walk before
  merging.

Negative controls:

| Change | Result |
| --- | --- |
| Event ring read with the wrong cycle bit | `FAIL usb`: both ports `error=command timed out`, `devices=0` |
| USB 2 port reset skipped | **not discriminating**: still passes, because QEMU's xHCI enables USB 2 ports without a reset. The reset path is untested until real hardware. |

## 3. Not done

- No input yet (H1b: configure the interrupt endpoints, boot protocol,
  keyboard and mouse reports into the existing keyboard and mouse
  queues).
- No hubs, no hot-plug, no interrupts, no USB 3 devices tested, no
  64-byte contexts tested (QEMU uses 32).
- Real hardware: untested. The port reset, firmware hand-off and
  scratchpad paths are written to the specification but QEMU does not
  exercise them.
