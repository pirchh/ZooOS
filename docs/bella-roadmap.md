# Bella roadmap: a baked-in model at GPU speed

Status: the owner's agreed direction, 2026-11-02. Copied from the owner's
planning document. It was reconciled with an outside review and a code
audit (docs/bella-audit.md). Bella is the assistant's name (the owner's
choice); the model is not fixed to Qwen, but its weights must be open
source.

## Summary

The model ships inside ZooOS and runs on whatever machine boots it, with
no network and no host OS.
- **The CPU path** is the floor that works on every PC.
- **The GPU** is an accelerator Bella uses when ZooOS has a driver for
  that card, starting with the owner's RTX 5060 Ti (16 GB).

Order of work:

0. **Fix** the data-loss and isolation bugs the audit found.
1. **Measure** speed and correctness, so each later stage proves its gain.
2. **Make it robust:** the model on the data disk, warm at start,
   supervised, never stalling the desktop.
3. **CPU speed:** XSAVE, CPU feature detection, an AVX2 backend.
4. **The action and event interface:** apps declare typed actions; Bella
   sees what happens.
5. **Bella v1:** one complete, safe, offline multi-step workflow.
6. **SMP:** every core.
7. **Hardware base and real disks:** ECAM, MSI-X, IOMMU, disk flushes,
   USB storage.
8. **GPU compute:** the RTX 3050 first, then the 5060 Ti, each milestone
   a stop point.
9. **Voice:** audio drivers, push-to-talk, local speech in and out.
10. **Model tiers:** the best model each PC can run.

## Why baking the model in is right

- **Portable.** A USB stick boots any PC with Bella working; there is
  nothing to pair, host or sign in to.
- **Private.** Requests, files and memory never leave the machine. The
  model has no file access and needs no network.
- **Always there.** No server to be down, no Wi-Fi to lose.

| Lever | Owning the OS helps? | Why |
| --- | --- | --- |
| Context gathering | Little | Microseconds on any OS; not where the time goes |
| Prompt length | Yes | Apps report compact, structured state, so prefill has fewer tokens |
| Model always warm | Yes | Resident model, cached system prompt, scheduler priority |
| Trust to act | Yes | The broker holds the capabilities; the model holds none |
| Arithmetic speed | No | Only hardware helps: wide vectors, more cores, a GPU |

**The cost:** compute becomes ZooOS's own problem. On macOS or Windows a
GPU driver comes free; here every speed-up is work. That is why the CPU
floor stays and the GPU is a bonus per card, not a dependency.

## Where the time goes today

From docs/a1-design.md and docs/a3-design.md, QEMU with WHPX:

| Stage | Time | Note |
| --- | --- | --- |
| Kernel up, everyday image with the model | 2.75 s | 1.85 s of it reads the 411 MB boot image |
| System prompt (55 tokens), first request | 3.44 s | Paid once per model service start |
| A request in words | about 1 s | Qwen2.5-0.5B, Q4_0 |
| Host prefill / generation | 13.2 / 9.8 tok/s | After A1 |
| Remembered request (A2) or typed command | no model wait | Broker only |

- **One core, baseline CPU.** The dev VM runs `-smp 1` with QEMU's base
  CPU model: no AVX2, no second core. These are the cheapest large gains.
- **The model is small for what it does now.** 0.5B is fine for choosing
  one of four tools; it will not plan several steps. A bigger model
  needs the speed first.

## Principles for every stage

- **The CPU path never goes away.** A missing or crashed GPU driver falls
  back to the CPU, never to "no Bella".
- **The model stays untrusted.** Faster or bigger, it holds no file,
  network or device handles; the broker does.
- **Answers must not change silently.** Every speed-up passes
  `infer-check` and `assist-check`, as A1 did.
- **Measure before and after.** Numbers go in docs/verification.md, `OK`
  only when actually run.
- **One backend interface.** CPU, AVX2, multi-core and GPU implement the
  same operations (matrix-vector, attention, norms), chosen at run time.

## Stages 0 to 2: fix, measure, make it robust

**Stage 0: fix what the audit found** (docs/s0-design.md).
- **Bugs:**
  - B1, Notes cuts files: never save a partial load.
  - B2, folder handle reuse: generations.
  - B6, bulk lengths: check against the mapping.
  - B7, sessions: a limit per client.
- **Memory (B3, B4, B8):**
  - learn only from results that found something;
  - never re-learn a recall;
  - run recalled commands through the command filter;
  - never recall `undo`, `quit` or `remember`;
  - the names in the request must match.
- **Supervision (B5):** restart limits decay, and a BadgerHandler restart
  gives the desktop a fresh home handle.
- **Docs (B15):** rewrite docs/m5c1-design.md.
- **Done when:** every new case is `OK` under TCG and the existing suite is
  unchanged.

**Stage 1: measure and evaluate.**
- One timing line per request: model parse, warm-up, prefill, decode,
  time to first token, tool run, key press to result.
- The peak heap of ElephantHandler and OtterHandler.
- `assist-check` gains out-of-scope and adversarial requests, a "none of
  these" outcome and a margin threshold (B9).
- A responsiveness check: from a key press in Files to its frame while
  the model runs.
- Baselines under TCG and WHPX in docs/verification.md.

**Stage 2: model lifecycle and responsiveness.**
- **The model:**
  - It moves from the boot image to the data disk. Measure the read: with
    today's polled disk it may cost more than it saves until the
    disk-lock item lands.
  - Warm the system prompt in the background at desktop start.
  - Restart ElephantHandler and OtterHandler with back-off.
- **Responsiveness:**
  - Non-blocking broker calls with a deadline and cancel (B14).
  - Disk requests stop holding the scheduler lock with interrupts off
    (B10).
  - Interactive and background priorities (B12).
  - A bounded kernel heap per process for channel memory (B13).
- **Rights:** the broker gets read, create and remove rights only, its own
  memory-file handle, and one audit line per action.
- **Done when:**
  - the first request is about as fast as later ones;
  - killing the model service leaves the desktop working;
  - responsiveness with the model running is close to idle.

## Stages 3 to 5: CPU speed and Bella v1

**Stage 3: XSAVE, CPU features and an AVX2 backend.**
- CPUID once at boot, with the feature bits given to user space.
- CR4.OSXSAVE and XCR0, the save area sized from leaf 0xD, and XSAVE and
  XRSTOR on task switches.
- One small trait at the `matvec` seam: today's code as the SSE backend,
  plus an AVX2 and FMA backend chosen at run time.
- Tests: wide registers kept across switches; backends agree within the
  0.02 logit limit.

**Stage 4: the action and event interface.**
- **Actions:**
  - A static action manifest per app beside `APPLICATIONS` (id, version,
    argument kinds, effect, confirmation).
  - Peacock routes `do:` invocations to apps, which reply with a
    structured result.
  - The broker asks for confirmation by effect.
- **Events:**
  - A subscriber list in Peacock for focus, open, close and minimise,
    dropping the oldest when a subscriber falls behind.
  - Bella keeps the last few events as compact context.
- **Capacity:** lift the fixed five-app arrays.

**Stage 5: Bella v1.**
- Fixed workflow templates (find then open; find, summarise, then create
  a note). The model picks a template and fills its slots; the broker
  runs and checks each step.
- Summarising is a separate model call with no tool choice. The text is
  marked as data, capped in size, and the result shown before anything
  is written.
- "None of these" asks the user instead of guessing.
- Try a larger model on AVX2; keep it only if accuracy rises and latency
  stays acceptable.
- Memory entries gain provenance, and a plain preferences file the user
  can edit.
- **Acceptance:**
  - offline, with no unauthorised access;
  - survives a service crash and works after a reboot;
  - handles variations at a measured success rate;
  - reports failures and writes audit lines.

## Stages 6 to 10

**Stage 6, SMP.**
- **Kernel:**
  - the ACPI MADT;
  - per-CPU data, TSS and stacks;
  - start the other cores;
  - replace the global locks;
  - TLB shootdowns;
  - targeted wake-ups;
  - recheck user-memory access.
- **Model:** split matrix rows across threads.

**Stage 7, hardware base and real disks.**
- **Devices:**
  - a shared PCI module;
  - ECAM, MSI-X, PAT write-combining;
  - a larger MMIO window;
  - an IOMMU.
- **Disks:**
  - disk flushes (B11);
  - USB mass storage, so a USB-booted PC keeps its files on the stick;
  - firmware loading from disk.

**Stage 8, GPU compute, the RTX 3050 first.** Each step is a stop point:
1. find the card;
2. boot its GSP firmware, following the Rust Nova driver;
3. GPU memory;
4. a trivial operation;
5. a matrix-multiply benchmark;
6. the model.

The 5060 Ti follows. Testing is on the test PC with the 3050 passed to
QEMU by VFIO, plus bare-metal USB boots. The driver's place (kernel or an
isolated handler) is decided in DECISIONS.md first.

**Stage 9, voice.**
- **Audio:** virtio-sound first, then Intel HD Audio; an audio service.
- **Speech:** push-to-talk, local speech recognition and a local voice.

**Stage 10, model tiers.** A measured self-test at boot picks among
models on the data partition by RAM, VRAM and speed.

## Risks and open questions

- **The 5060 Ti is in the dev PC,** where ZooOS must never boot.
  - GPU bring-up therefore happens on the test PC (i7-11700K, RTX 3050):
    a Linux host, with the card passed to QEMU by VFIO.
  - The 5060 Ti later needs the card moved there for a while, or a
    deliberate, documented exception.
- **SMP removes safety assumptions.** The kernel reads user memory after
  checking it, which is safe only while no other thread can run.
- **NVIDIA bring-up can take many months.** Stages 0 to 7 deliver value
  without it.
- **Firmware licence.** The GSP firmware is NVIDIA's binary; check the
  licence before shipping it.
- **CPU-only PCs stay slow for big models.** Tiers keep Bella usable.
- **Image size.** Several models can mean several GB; size the data
  partition for it.
