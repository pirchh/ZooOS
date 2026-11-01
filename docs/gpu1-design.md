# GPU step 1 (GPU1): virtio-gpu display and a hardware cursor

Status: implemented 2026-11-01 (version 0.56.0). After U1c the owner
still felt a slight lag in QEMU and asked for the engineer's next step.

## 1. Why

ZooOS drew its pointer into a plain framebuffer (QEMU's standard VGA).
QEMU copies such a framebuffer to its window on its own refresh timer,
so every pointer move waited for that copy. Windows and macOS move the
pointer with the GPU's cursor plane instead: no redraw, shown at once.

QEMU's virtio-gpu offers the same thing:
- **Updates:** the guest says which rectangle changed (transfer and
  flush), and QEMU shows it at once, with no refresh-timer scan.
- **Cursor:** a separate cursor resource. QEMU hands its image to the
  host window system as the real host cursor, so under QEMU the ZooOS
  pointer is the Windows pointer, drawn by Windows.

Not measured: QEMU's own window latency, which the guest cannot see.
What the guest can see is in §4.

## 2. The device and the driver

- **The device:** QEMU's `virtio-vga` is a VGA device with a virtio-gpu
  behind it.
  - The firmware, the bootloader and the kernel console use the VGA side
    unchanged.
  - `xtask run` and the `gpu-` cases use it, with
    `xres=1920,yres=1080,edid=on`.
  - Every other case stays on the standard VGA, the path real hardware's
    firmware framebuffer takes.
- **The driver** (`kernel/src/virtio_gpu.rs`, modern virtio PCI
  transport):
  - **At boot:** it finds the device (1AF4:1050) and maps its common and
    notify configuration from the vendor capabilities. It negotiates
    `VIRTIO_F_VERSION_1` only and sets up the control and cursor queues
    (16 entries, polled, no interrupts). From `GET_DISPLAY_INFO` it takes
    scanout 0's preferred size, kept between 1920×1080 and 1920×1200.
    QEMU's GTK window reports its own size as the preferred mode, 640×480
    before any guest mode is set; the headless test machines report the
    configured 1920×1080. It allocates the screen (one
    contiguous block of RAM) and a 64×64 cursor.
  - **When a process takes the display:** `DISPLAY_ACQUIRE` creates the
    screen resource (B8G8R8X8) and the cursor resource (B8G8R8A8),
    attaches their memory and sets scanout 0. The screen's frames are
    mapped into the owner instead of the firmware framebuffer, and
    `DisplayInfo::flags` says `DISPLAY_FLUSH_NEEDED | DISPLAY_HW_CURSOR`.
  - **When the owner exits:** the device is reset, which puts
    `virtio-vga` back in VGA mode where the console draws, and is
    brought up again for the next owner.
- **Requests** are synchronous: up to four per batch, each waiting
  (bounded) for the device.

## 3. New system calls and the window server

- **`DISPLAY_FLUSH` (66):** `TRANSFER_TO_HOST_2D` then `RESOURCE_FLUSH` of
  one rectangle. PeacockHandler calls it for each rectangle it presents.
- **`CURSOR_SET` (67):** a 64×64 0xAARRGGBB image, its hot spot and its
  position. PeacockHandler sends the same arrow it draws, once at start.
- **`CURSOR_MOVE` (68):** after each batch of mouse events in which the
  pointer moved. A pointer move then damages nothing and draws nothing.

The window server keeps its software pointer for displays without these
flags: the firmware framebuffer on real hardware, and standard VGA.

## 4. Measurements (WHPX)

| | Standard VGA | virtio-gpu |
| --- | --- | --- |
| Full repaint, flush included (`frames`, `gpu-frames`, 43 frames, three runs each) | 8.5–11.0 M cycles | 9.9–10.2 M cycles |
| Frames drawn during the 40-move pointer sweep | 40–43 | 3 (the menu-bar clock ticking) |
| One pointer frame | about 60,000 cycles | none (a `CURSOR_MOVE`) |

A control that drew the pointer in software on virtio-gpu measured
128,692 cycles per pointer frame, the flush included. So the cursor
plane also avoids that cost.

## 5. Not done

- **Real hardware:** there is no virtio-gpu there. ZooOS uses the
  firmware framebuffer with a software pointer, and scanout reads it
  directly, so the QEMU refresh-timer copy does not exist. A cursor plane
  on real GPUs needs a driver per GPU family (Intel, AMD, NVIDIA): later
  work.
- **Asynchronous requests:** each flush waits for QEMU. Measured above as
  no extra cost, so left simple.
- **Display hotplug and resizing the QEMU window to a new mode:** not
  handled. The size is fixed at boot.
