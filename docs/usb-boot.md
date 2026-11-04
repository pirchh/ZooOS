# Booting ZooOS from a USB stick (owner's other PCs only)

Status: hardware step 2a (H2a), 2026-10-31, version 0.32.0. Nothing here
has been run on real hardware yet; this records what is expected and what
could go wrong, so the first real test is short and safe.

## The rule

**Never on the development PC** (the Windows machine this repository lives
on): no booting, installing or testing ZooOS there, from any disk or USB,
and no changes to its disks or boot settings (AGENTS.md, owner rule
2026-10-30). Real hardware means one of the owner's other PCs, booted from
a USB stick.

## Making the stick

1. `cargo xtask usb-image` builds the everyday image (fast boot, starts in
   the desktop) and copies it to `build/usb/zoo-os-usb.img`, printing its
   size and SHA-256. It writes no device. Since 0.101.0 it includes
   Bella's chat model (Qwen3-1.7B, 4 bits) and the embedding model when
   `tools/models/` has them: 1.8 GB, so **a stick of 2 GB or larger and
   a PC with at least 4 GB of memory** (docs/usb-model-design.md);
   `--no-model` builds the small image. The stick keeps the models in
   its boot image: `cargo xtask run` puts them on a separate model disk
   instead (docs/stage2-design.md §3), but ZooOS has no USB storage
   driver yet to read such a disk on a real PC.
2. The owner writes that file to a USB stick they choose, with a tool such
   as Rufus (DD mode) or balenaEtcher, **double-checking the target is the
   stick**. Everything on the stick is erased. The agent never writes to a
   device.
3. On the other PC: boot menu (often F12, F11, F8 or Esc at power-on),
   choose the stick's UEFI entry. Secure Boot must be off (the image is
   not signed).

## What should work

- UEFI boot through the bootloader crate, the framebuffer from the
  firmware (GOP), the desktop. Screens larger than 1920×1200 (common on
  real monitors) use the top-left 1920×1200 area, the rest painted black;
  before H2a the desktop refused them. Tested in QEMU at 2560×1600 (the
  `big-screen` case: a test-only mode whose disk image's boot
  configuration asks the firmware for a screen at least 2000 pixels
  wide).
- USB keyboard and mouse through xHCI (docs/h1-design.md,
  docs/h1b-design.md) on the PC's own ports, and PS/2 where it exists.
- Everything in memory: Files, Notes, Bella's typed commands.

## Known risks (in rough order of likelihood)

| Risk | Effect | Next step |
| --- | --- | --- |
| ~~The timer is the legacy PIT, which some recent Intel platforms gate~~ | addressed in H3 (docs/h3-design.md): the local APIC timer, calibrated against the ACPI PM timer, is the tick; the PIT is only a fallback | — |
| No disk driver for real disks (only QEMU's virtio) | files do not persist (`disk none`); Notes saves to memory only | USB mass storage on the stick itself (safer than touching internal disks) |
| Keyboard/mouse behind a USB hub (common for front ports and some laptops' internal keyboards) | one tier of USB 2 hubs supported since H4 (docs/h4-design.md); hubs behind hubs, USB 3 hubs and high-speed hubs' translators are untested | report the PC model if input is missing |
| USB 2 port reset and firmware hand-off paths are untested (QEMU skips them) | a device on that port is not enabled (`error=port-…`) | debug with the serial log or the screen |
| Laptop keyboards on i2c/eSPI, touchpads | no input on that laptop | try a USB keyboard and mouse |
| No ACPI: no power-off, no sleep | switch off with the power button | ACPI later |
| The xHCI interrupt (MSI-X or MSI, since U1c) never arrives on that PC | input still works through the 10 ms tick poll, a little less responsive | report the `usb interrupts=` line |

## What to report back

A photo of the screen if it stops, or the last lines shown, and the PC's
model. The kernel prints every step (`ZOO_OS ...` lines) on screen until
the desktop starts.
