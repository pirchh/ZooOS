# Feasibility study: accelerated graphics on ZooOS

| Field | Value |
| --- | --- |
| Date | 2026-10-30 |
| ZooOS version | 0.6.0 (after M4) |
| Gates (COMPATIBILITY.md) | GPU; Graphics translation |
| Procedure | Desk study of how GPU drivers reach non-Linux systems and what the WoW path needs, compared with ZooOS today and with the QEMU-on-Windows test setup. No code was run. |
| **Status** | **BLOCKED** for both gates: no GPU driver path exists on ZooOS, and the current QEMU-on-Windows setup cannot test 3D acceleration (§5). |

## 1. Question

What would ZooOS need for "working accelerated graphics" for World of Warcraft
(ARCHITECTURE.md calls GPU acceleration a critical feasibility gate), and can
it be developed and tested with the current setup?

## 2. The chain WoW needs

```
WoW (Direct3D 11 / 12)
  └─ DXVK (D3D 8–11 → Vulkan) / vkd3d-proton (D3D 12 → Vulkan)    [run under Wine]
      └─ Vulkan driver, user space: Mesa RADV (AMD), ANV (Intel), NVK (NVIDIA)
          └─ kernel GPU driver: Linux DRM (amdgpu, i915/xe, nouveau/nova)
              └─ PCI device, MMIO, interrupts, DMA/IOMMU, firmware blobs
```

On Linux, Proton uses exactly this chain ([Proton](https://en.wikipedia.org/wiki/Proton_(software))).
As COMPATIBILITY.md says, **a Vulkan library or translation layer alone is not
a GPU driver.** Every layer below the Vulkan API is missing on ZooOS, and
the layers above it also need Wine (wine-hosting.md).

## 3. How other systems got GPU drivers

| System | Approach | Lesson |
| --- | --- | --- |
| FreeBSD | **LinuxKPI**, a Linux kernel-API compatibility layer, plus `drm-kmod`: Linux DRM drivers (amdgpu, i915, radeon) taken almost unmodified, Linux-specific changes guarded, shims preferred over driver patches. Has tracked Linux DRM since about 4.9 (2018). | Porting DRM means emulating a large part of the Linux kernel API (memory, locking, workqueues, DMA, PCI, firmware loading). It works, but it is an ongoing maintenance stream. Sources: [FreshPorts drm-kmod](https://freshports.org/graphics/drm-kmod), [Phoronix](https://www.phoronix.com/news/DRM-Next-KMOD-On-FreeBSD-11), [drm-kmod mirror](https://github.com/ravynsoft/drm-kmod). |
| Haiku | Own Radeon kernel driver plus Mesa's RADV; OpenGL through Zink over Vulkan (2021, Blender tested). Older Radeon cards have no 3D. A 2026 report of an early NVIDIA/NVK driver is unverified. | A non-Linux OS can run Mesa's Vulkan drivers, but only after a POSIX environment, Mesa porting and a kernel driver, over several years. Sources: [Phoronix: Zink on RADV](https://www.phoronix.com/news/Haiku-OS-Zink-RADV), [Haiku forum](https://discuss.haiku-os.org/t/ati-hd-4850-and-3d-acceleration/18726), [WebProNews (unverified)](https://www.webpronews.com/haiku-os-releases-first-open-source-nvidia-graphics-driver-v0-0-1). |

## 4. What ZooOS would need

| Layer | Needs | ZooOS 0.6.0 |
| --- | --- | --- |
| Hardware access | PCI enumeration, mapping device memory (MMIO), MSI/MSI-X interrupts through the local APIC, DMA with IOMMU isolation, firmware loading | none of these: legacy PIC only, no PCI, no DMA policy (DECISIONS.md: document interrupt, DMA and ownership boundaries before loading drivers) |
| Kernel GPU driver | Either a LinuxKPI-style layer hosting Linux DRM (FreeBSD's way), or a new native driver for one GPU | none |
| Driver interface | Buffer objects shared between processes, mapping GPU memory into user space, fences/sync objects, command submission, GPU reset | channels only; no shared memory; no fd-like objects |
| User space | Mesa's Vulkan driver (C/C++, threads, `mmap`, libdrm-style ioctls), a Vulkan loader | no libc, threads or memory mapping (same blockers as wine-hosting.md) |
| Display | Presenting GPU-rendered surfaces (modesetting, scan-out) | window server copies a CPU back buffer to the UEFI framebuffer |

**Firmware and licences:** amdgpu needs AMD's binary firmware (distributed in
`linux-firmware` under a redistribution licence); Linux DRM is mostly MIT
or GPL-2.0 per file. A licence inventory is required before reuse
(COMPATIBILITY.md).

## 5. Can it be tested in the current setup?

- ZooOS is developed and verified in **QEMU 11.1 on a Windows host with
  software emulation (TCG)**. QEMU's emulated display is a plain framebuffer.
- QEMU's 3D options (virtio-gpu with virgl for OpenGL, or Venus for Vulkan)
  need host OpenGL/Vulkan support in QEMU's display backend. **No evidence
  was found that stock upstream QEMU provides virgl or Venus on Windows
  hosts.** A QEMU issue tracks it, and a 2024 user report fell back to GPU
  passthrough. Sources: [qemu-devel 2021](https://lore.kernel.org/qemu-devel/CAOEv1kYzRWDzpzgMt0bGDEVKrEjDYXTJNqYdsXj+8f2L6uYieA@mail.gmail.com/T/),
  [QEMU issue 564](https://gitlab.com/qemu-project/qemu/-/issues/564),
  [user report](https://groups.google.com/g/anitigeph/c/LnLbz0pS1XM),
  [Venus announcement](https://www.gamingonlinux.com/2021/12/collabora-announced-venus-3d-accelerated-vulkan-in-qemu/).
- **Paths to an acceleration test bed, each needing an owner decision:**
  a Linux development host (QEMU with Venus, or KVM with GPU passthrough); a
  dedicated physical reference machine (AGENTS.md forbids testing on the
  user's own machine); or Windows-host GPU passthrough (host configuration
  change). None is in the current setup.
- QEMU's software CPU emulation also cannot establish game performance
  (COMPATIBILITY.md).

## 6. Outcome and next decision

- **GPU gate: BLOCKED.** No hardware-access layer, no driver, no driver
  interface, and no way to test acceleration in the current QEMU-on-Windows
  setup.
- **Graphics translation gate: BLOCKED** by the GPU gate and the Wine gate.
- **Recommended reference GPU, if pursued:** a current AMD Radeon (open
  kernel driver and RADV, public documentation, redistributable firmware).
  NVIDIA's proprietary driver cannot be ported. Its open drivers (NVK,
  nova) are young.
- **Owner decisions needed (scope, host and hardware):**
  1. Whether to keep WoW with accelerated graphics as a goal, knowing it
     requires a POSIX environment, a LinuxKPI-style layer or native driver,
     Mesa and Wine: a long multi-milestone programme.
  2. If kept: which reference GPU and test bed (Linux host, or a dedicated
     physical machine).
  3. Otherwise: narrow the graphics goals (e.g. software-rendered native
     apps first) and revisit later.
