# More than one processor

Status: implemented, steps 1 to 3. Step 1 is in version 0.96.0; steps 2
and 3 are in 0.97.0. The owner's goal is answers in under a second
(2026-11-03), and their PC has 16 cores (an i9-12900K), while ZooOS used
one.

| Step | What | Status |
| --- | --- | --- |
| 1 | Find the processors, start them, give each its own GDT, TSS and local APIC, park them | done, 0.96.0 |
| 2 | Run tasks on every processor: one kernel lock, a current task per processor, idle loops, TLB shootdown, Linux programs on processor 0 | done, 0.97.0 |
| 3 | The model's matrix work split across processors (worker threads) | done, 0.97.0 |

**`cargo xtask run` uses 8 processors** (`--cpus` changes it).
`cargo xtask verify` uses 1 unless `--cpus N`; `smp-` cases use at least 4.

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

## Step 2: tasks on every processor (as built)

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

What it took, beyond the plan:

- **Locks wait, and flush meanwhile.** `lock_sched` and `memory::with`
  wait while another processor holds the lock. While waiting, they serve a
  flush request (`smp::service_flush`): a processor waiting with
  interrupts off would otherwise never acknowledge a shootdown, and two
  processors would wait on each other. Re-entry on the same processor
  still panics with a clear message.
- **Idle.** A non-boot processor with nothing to run is in its idle loop
  (`Scheduler::current` is `AP_IDLE`): `sti; hlt`, then `int 0x81` to offer
  itself. `activate(AP_IDLE)` goes back to the loop's saved frame, on the
  kernel's own table. A processor with nothing to do is woken by
  `WAKE_VECTOR` when a dispatch leaves a ready task that none runs
  (`unclaimed_ready`, `smp::wake_idle`).
- **The stack hand-off.** This was the first bug found: two processors
  crashed on one kernel stack. A processor switching away from a task
  still runs on that task's kernel stack until `trap_common` loads the
  next one, but it had already released the lock, and another processor
  could resume the task on the same stack. Now the task stays in hand
  (`smp::leaving`, `is_leaving`) until `trap_common` calls
  `smp::after_switch` on the new stack. A first version cleared it only
  with more than one processor online, which hung a thread forever on a
  single processor; it always clears now.
- **Per processor:** a TSS (`gdt::set_kernel_stack` writes this
  processor's), the local APIC timer at the BSP's calibration, the page
  table it runs on (`smp::set_cr3`, for shootdowns), and a switch count
  (the shell's `cpus`).
- **The boot processor only:** ticks are counted and devices polled
  there (other processors only acknowledge their timer); the kernel
  task; Linux programs (`pick_ap` skips them).
- **The serial log keeps its order.** A line is formatted whole, then
  written in one piece (`console::_kprintln`). A processor that finds the
  output buffer busy waits for it, instead of writing past the buffer,
  which put lines out of order and mixed others' text into the middle of
  a line. Only a fault on the processor that holds it writes directly.

## Step 3: the model's matrix work split

- **`zoo_infer::par`.** The engine has no threads, so it asks a `Pool`
  that a program installs: a "parallel for" over jobs. `par::fill`
  computes `out[t * rows + r] = value(r, t)` with the rows split into
  `4 × width` jobs. Products of less than 256 KiB of weights are not
  split. `matvec` and `matmul` use it, and the dot products per row are
  unchanged, so results are identical; the host test `fill_matches_serial`
  checks it against a pool on std threads.
- **`zoo_rt::pool::Workers`,** the guest's pool:
  - one worker thread per other processor;
  - each batch has a generation, and a worker claims job indices by
    compare-and-swap on one word holding the generation and the next
    index, so a worker that wakes late cannot run a job of a batch that
    has moved on;
  - workers spin about 20,000 times between batches, then sleep on a
    futex.
- **`CPU_COUNT` (78):** processors online. ElephantHandler installs the
  pool when there is more than one.

## Measured (WHPX, whole reply; first words in brackets)

| Request | 0.5B, 1 | 0.5B, 4 | 0.5B, 8 | Qwen3 `Q4_0`, 1 | Qwen3 `Q4_0`, 4 | Qwen3 `Q4_0`, 8 |
| --- | --- | --- | --- | --- | --- | --- |
| Small talk | 0.61 s (0.17) | — | **0.25 s (0.07)** | 2.55 s (1.40) | 2.71 s (1.49) | **0.78 s (0.43)** |
| From a fact | 0.93 s (0.54) | 0.39 s (0.22) | **0.30 s (0.18)** | 3.49 s (2.22) | 1.72 s (1.14) | **1.32 s (0.86)** |
| Summarise `Documents/notes.txt` | 1.67 s (1.27) | 1.02 s (0.78) | **0.68 s (0.52)** | 13.8 s (10.4) | 6.33 s (4.23) | **4.42 s (2.97)** |
| Warm-up, system prompt | 2.8 s | 1.6 s | 1.1 s | 14.0 s | 6.5–18.3 s | 5.2 s |

With 8 processors **the default model answers every case in under a
second**, and Qwen3 answers small talk in 0.78 s.

The runs vary. One Qwen3 4-processor small-talk run was no faster than one
processor, with an 18.3 s warm-up. The host was busy with other QEMU
runs, and WHPX schedules each virtual processor as a host thread. The
0.5B small-talk row with 4 processors failed in this sweep, on the log
mixing fixed since (`smp-boot` and `bella-talk` then passed 3 runs in 3).
