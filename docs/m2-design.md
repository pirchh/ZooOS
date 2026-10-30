# M2 design: kernel foundations

Status: design written before implementation, as MILESTONES.md requires,
then implemented and verified (2026-10-30). Results are in
docs/verification.md. Sections marked *Deferred* are not part of M2. Where
implementation changed a detail, the text below has been updated and says so.

Scope: memory ownership, page mappings, a physical frame allocator, a kernel
heap, CPU tables (GDT/TSS/IDT), exception handling, and a periodic timer.
Still single CPU. No processes, scheduler, syscalls, filesystem or graphics.

## 1. Facts this design relies on (bootloader 0.11.17, checked in source)

- The bootloader maps an unmapped **guard page directly below the kernel
  stack** and reports the stack as `BootInfo.kernel_stack_bottom` (lowest
  mapped address) and `kernel_stack_len`.
- It sets **CR0.WP** (supervisor writes respect read-only pages) and
  **EFER.NXE** (no-execute bits are honoured) before jumping to the kernel.
- With `mappings.physical_memory = Some(Mapping::Dynamic)` it maps all
  physical memory at an offset reported in `BootInfo.physical_memory_offset`.
- Dynamic mappings are placed only inside
  `[dynamic_range_start, dynamic_range_end]`, chosen per level-4 entry.
- Page-table frames, the boot info, kernel image and stack are reported as
  `MemoryRegionKind::Bootloader`; only `Usable` regions are free.

## 2. Memory ownership

| Owner | What | Rule in M2 |
| --- | --- | --- |
| Frame allocator | Physical frames inside `Usable` regions at or above 1 MiB | Hands out and takes back 4 KiB frames. Sole owner of free physical memory. |
| Bootloader (preserved) | `Bootloader` regions: page tables, kernel image, stack, boot info | Never reused or freed in M2. |
| Firmware (preserved) | `UnknownUefi` regions (runtime services, ACPI, etc.) | Never touched. |
| Below 1 MiB | Legacy/real-mode area | Never allocated, even if reported usable. Frame 0 is never handed out. |
| Kernel page mapper | Level-4 entries 256–511 (upper half) | Kernel-owned. The bootloader is configured never to place dynamic mappings there. |
| Bootloader mappings | Level-4 entries 0–255 (lower half) | Kernel image, stack, boot info, framebuffer, physical-memory window. Preserved as-is. |

There is exactly one frame allocator and one page-table mapper. Both are
owned by a single `Memory` value created in `kernel_main` and passed by
`&mut`. No global mutable memory state, so no lock is needed. Interrupt and
exception handlers never touch either.

*Deferred:* reclaiming `Bootloader` regions, freeing intermediate page-table
frames, NUMA, huge pages, per-CPU state.

## 3. Virtual address layout

Upper half (kernel-owned), all 4 KiB pages:

| Range | Use | Flags |
| --- | --- | --- |
| `0xFFFF_C000_0000_0000` – `+0x1000` | heap lower guard page | unmapped |
| `0xFFFF_C000_0000_1000` – `0xFFFF_C000_0010_1000` | kernel heap, 1 MiB | present, writable, no-execute |
| `0xFFFF_C000_0010_1000` – `+0x1000` | heap upper guard page | unmapped |
| `0xFFFF_C000_4000_0000` | scratch page 0: read-only fault test | present, no-execute (not writable) |
| `0xFFFF_C000_4000_1000` | scratch page 1: map/unmap self-test | mapped and unmapped by the test |

Lower half: whatever the bootloader chose (reported at boot in `INFO` lines).

At start-up the kernel checks that level-4 entries 256–511 are all empty
before mapping anything there. If not, it panics with a clear message rather
than overwrite unknown mappings.

## 4. Physical frame allocator

- A bitmap, one bit per 4 KiB frame from address 0 up to the end of the
  highest `Usable` region (512 MiB guest → 16 KiB bitmap).
- The bitmap is stored in frames taken from the first `Usable` region large
  enough, accessed through the physical-memory window; those frames are
  marked used.
- A bit is "free" only for frames wholly inside a `Usable` region, at or
  above 1 MiB, and not holding the bitmap.
- `allocate() -> Option<PhysFrame>` (first fit with a rotating search hint).
- `deallocate(frame) -> Result<(), FrameError>` rejects frames it does not
  manage (`NotManaged`) and frames already free (`DoubleFree`). Errors are
  reported, never ignored.
- Counters for total and free frames back the self-tests.
- Implements `x86_64::structures::paging::FrameAllocator<Size4KiB>` so the
  mapper can take frames for new page tables. Those frames are not returned in
  M2.

## 5. Page mappings

- `x86_64::structures::paging::OffsetPageTable` over the active level-4 table,
  using `physical_memory_offset`. Created once.
- Unsafe assumptions, stated where it is created: the whole of physical memory
  is mapped at that offset, the active CR3 table is the one passed in, and no
  other reference to it exists.
- Mapping always flushes the TLB entry for the changed page. Unmapping returns
  the frame to the caller, who frees it.

## 6. Kernel heap

- `linked_list_allocator::LockedHeap` registered as `#[global_allocator]`,
  enabling `alloc` (Box, Vec, String). Fixed 1 MiB; it does not grow in M2.
- Initialised only after its pages are mapped. An allocation before that, or
  running out, fails, and Rust's default `no_std` handling turns that into a
  panic, which the panic handler reports.
- Rule: interrupt and exception handlers do not allocate (the heap lock is a
  spin lock and a handler could interrupt a holder).
- The pages on either side of the heap are left unmapped as guard pages.

## 7. CPU tables, exceptions and the timer

- **GDT**: kernel code segment, kernel data segment and a TSS. CS is reloaded
  and SS/DS/ES are set to the data segment; then the TSS is loaded.
- **TSS**: interrupt stack table entry 0 is a dedicated 20 KiB double-fault
  stack (a static array), so a kernel stack overflow can still be reported.
- **IDT** handlers, written with the `x86-interrupt` calling convention
  provided by the `x86_64` crate (requires the unstable
  `abi_x86_interrupt` feature; the toolchain is already a pinned nightly and
  this avoids hand-written assembly entry stubs):
  - breakpoint (#BP): counted and returns (used by a self-test).
  - divide error, invalid opcode, segment not present, stack-segment fault,
    general protection, page fault, alignment check, machine check, SIMD:
    report and halt.
  - double fault: runs on the IST stack, reports and halts. If CR2 lies in the
    kernel-stack guard page, the report says `stack-guard-hit`.
  - Vectors without a handler raise #GP on use, which is reported.
- **Interrupt controller**: legacy 8259 PICs, remapped to vectors 32–47, all
  lines masked except IRQ0 (timer). Spurious IRQ7/IRQ15 are recognised and
  not acknowledged incorrectly. *Deferred:* local APIC / IOAPIC (needed for
  SMP and later hardware).
- **Timer**: PIT channel 0, mode 2, divisor 11932 (≈100 Hz). The handler
  increments an atomic tick counter and sends end-of-interrupt. No scheduling.
- Interrupts are enabled only after the GDT, IDT, PIC and PIT are set up. The
  panic handler and fatal exception handlers disable interrupts before halting.
  A normal kernel idles with interrupts on (`sti; hlt`).

**No silent reboot:** every fatal path prints a marker and halts. QEMU runs
with `-no-reboot`, so a triple fault ends QEMU, which the verifier reports as
an early exit (a failure).

## 8. Diagnostic markers

Fatal exceptions print one marker line, then `INFO` details:

    ZOO_OS EXCEPTION page-fault access=<read|write|execute> cause=<not-present|protection> addr=0x<16 hex digits>
    ZOO_OS EXCEPTION double-fault stack-guard-hit        (CR2 in the stack guard page)
    ZOO_OS EXCEPTION double-fault cr2=0x<16 hex digits>  (otherwise)
    ZOO_OS EXCEPTION <name>                              (other exceptions)
    ZOO_OS INFO exception rip=0x… error=0x… …

Self-tests print `ZOO_OS TEST <name> ok` on success. On failure they
panic with `self-test <name>: <reason>`, which the verifier rejects.

## 9. Build modes and verification cases

Each mode is a separate kernel build with its own Cargo feature, target
directory and image, as in M1. Features are mutually exclusive (enforced by
`compile_error!`). All modes run CPU and memory initialisation first.

| Mode (feature) | Expected markers after `BOOT` and `INFO mode=…` | Proves |
| --- | --- | --- |
| normal | `TEST breakpoint ok`, `TEST frames ok`, `TEST paging ok`, `TEST heap ok`, `TEST timer ok`, `READY` | exception return, allocation/free correctness, mapping/unmapping, heap reuse, timer interrupts |
| panic (`panic-test`) | same five `TEST` lines, then `PANIC intentional-test` | panic reporting with interrupts on |
| fault-write-protect | `EXCEPTION page-fault access=write cause=protection addr=0xffffc00040000000` | protection fault on a read-only page |
| fault-heap-guard | `EXCEPTION page-fault access=read cause=not-present addr=0xffffc00000101000` | heap guard page |
| fault-stack-overflow | `EXCEPTION double-fault stack-guard-hit` | kernel-stack guard page and IST stack |
| triple-fault (negative) | none; QEMU must exit | the verifier rejects a reboot/triple fault as `early-exit` |

Negative cases kept from M1: the panic image under normal expectations
(`unexpected-marker`) and a never-emitted marker (`timeout`).

### Self-test details (normal and panic modes)

- **breakpoint**: `int3` must reach the handler exactly once and return.
- **frames**: allocate 64 frames; all distinct, aligned, ≥ 1 MiB, inside
  `Usable` regions; a distinct pattern written to each through the
  physical-memory window reads back intact; after freeing all, the free count
  equals its starting value; a second free of the same frame returns
  `DoubleFree`; freeing frame 0 returns `NotManaged`; allocating 64 again
  succeeds.
- **paging**: scratch page 1 is unmapped; map it to a fresh frame; a value
  written through the virtual address is seen through the physical window;
  unmap it, check it no longer translates, free the frame. Also confirm that
  both heap guard pages and the kernel-stack guard page do not translate.
- **heap**: Box and Vec work; a 4096-byte-aligned allocation is aligned;
  allocating and dropping 12 MiB in total (2048 × 4 KiB plus 8 × 512 KiB, 12×
  the heap) succeeds, which proves memory is reclaimed; heap usage returns to
  its starting value.
- **timer**: with interrupts on, at least 10 ticks arrive within a bounded
  spin (otherwise panic `self-test timer: …`).

## 10. Dependencies (proposed pins)

| Crate | Version | Why |
| --- | --- | --- |
| x86_64 | 0.15.5 | Page tables, GDT/IDT/TSS types, port I/O, control registers. Already used by the bootloader. Latest stable (0.16 is a release candidate). |
| pic8259 | 0.11.0 | 8259 initialisation and EOI. |
| linked_list_allocator | 0.10.6 | Small, established `no_std` heap. |
| spin | 0.12.3 | `LazyLock` for the static GDT/TSS/IDT, `Mutex` for the PICs. (0.10.0 was first proposed but is yanked.) |

Pinned exactly (`=`) in kernel/Cargo.toml. The kernel's own inline assembly is removed: port I/O and `hlt` come from
`x86_64`. Remaining `unsafe` blocks each carry a `SAFETY:` comment.

## 11. Explicitly deferred to later milestones

Scheduler, threads, processes, address spaces per process, user mode,
syscalls, IPC, APIC/IOAPIC, SMP, ACPI parsing, heap growth, reclaiming
bootloader memory, keyboard and other drivers, framebuffer output.
