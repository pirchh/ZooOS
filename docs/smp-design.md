# More than one processor

Status: step 1 of 3 implemented in version 0.96.0: the other processors
start and park. The owner's goal is answers in under a second
(2026-11-03), and their PC has 16 cores (an i9-12900K), while ZooOS used
one. Steps 2 and 3 are proposals until they land.

| Step | What | Status |
| --- | --- | --- |
| 1 | Find the processors, start them, give each its own GDT, TSS and local APIC, park them | done, 0.96.0 |
| 2 | Run tasks on every processor: one kernel lock, a current task per processor, idle loops, TLB shootdown, Linux programs on processor 0 | proposed |
| 3 | The model's matrix work split across processors (worker threads) | proposed |

## Step 1: starting the processors

**Finding them.** `apic::processors` reads the ACPI MADT (signature
`APIC`, found through the RSDP the bootloader passes, as the FADT already
was). It lists each enabled or online-capable processor-local-APIC entry's
id. Every id but the boot processor's gets started.

**Low memory.** SIPI starts a processor in real mode, at a page below 1
MiB. `smp::set_low_pages` takes four whole `Usable` pages between 0x1000
and 0x9F000 from the bootloader's map, which the frame allocator never
hands out (`LOW_MEMORY_LIMIT`):
1. the trampoline;
2. a PML4,
3. a PDPT and
4. a PD for the temporary page tables.

**The trampoline** (`smp.rs`, `global_asm!`, about 50 instructions) is the
only new assembly:

1. **Real mode, at `T:0`.** Load a tiny GDT (32-bit code, data, 64-bit
   code) and turn on protected mode.
2. **32-bit.** PAE, then the temporary tables in CR3, then `EFER.LME` and
   `EFER.NXE` (the kernel's tables use NX), then paging and write
   protection.
3. **64-bit.** Load the stack, the processor number and the entry point
   from fields after the code (RIP-relative, so the copy works at any
   page), then jump to `ap_entry`.

The temporary tables map the first 2 MiB to itself, where the trampoline
runs while paging comes on, and copy the kernel's upper half (PML4
entries 256–511), where all of the kernel lives: code, statics, heap, the
physical-memory map. The BSP fills in each field before each start
(`start_one`): the GDT base, both far-jump targets, CR3, and the
processor's stack, number and entry.

**Starting each processor** uses INIT, 10 ms, then SIPI up to twice, 200 µs
apart, the usual sequence. The BSP waits up to 100 ms for it to report
before starting the next. `apic::delay_us` waits on the ACPI PM timer.

**`ap_entry`** runs on the new processor:
1. It loads the kernel's own CR3.
2. It builds its own GDT and TSS (`gdt::init_ap`; the same selectors, its
   own double-fault stack).
3. It loads the shared IDT and sets up its FPU registers (`fpu::init`).
4. It enables its local APIC with the timer masked (`apic::init_ap`).
5. It logs `ZOO_OS INFO cpu N online apic=A`, then halts with
   interrupts off.

The BSP logs `ZOO_OS INFO smp cpus=N`.

## Checks

**`smp-boot`** (4 processors):
- `cpu 1 online`, `cpu 2 online`, `cpu 3 online`, then `smp cpus=4`;
- then the normal boot to `ZOO_OS SHELL ready`.

It passes under TCG and WHPX. Every other case still boots with one
processor (`qemu::set_cpus`, 1 unless a case's name starts with `smp-`).

**Negative control:** SIPI pointed one page off, at no trampoline. The
processor ran garbage and reset the machine, and the case gave `FAIL`
(`early-exit`). A broken start-up path is caught, not hidden.

## Step 2, proposed: tasks on every processor

- **One kernel lock.** The scheduler lock becomes a spin lock that any
  processor may wait for, with re-entry on the same processor still a
  panic. Programs run in parallel on all processors; kernel code runs on
  one processor at a time. Inference spends nearly all its time in user
  code, so this is where the gain is, and every kernel path keeps its
  one-processor reasoning.
- **A current task per processor.** `Scheduler::current` is swapped in
  from a per-processor slot each time a processor takes the lock, so the
  10 places that read it do not change. `pick_next` skips tasks running
  elsewhere.
- **Per processor:** an idle task, the local APIC timer (the BSP's
  calibration), and RSP0 in its own TSS.
- **Linux programs stay on processor 0:** their `syscall` entry uses one
  scratch stack variable.
- **TLB shootdown.** When a mapping is removed or reduced in an address
  space that another processor is running, that processor is told by IPI
  to flush, and acknowledges before the frames are reused. A process that
  ends is not freed while any of its threads still runs elsewhere.
