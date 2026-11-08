# Initial decisions

Status: initial implementation defaults, with feasibility checks noted below.

## Settled for M0/M1

- Independent kernel, not a Windows or Linux kernel fork.
- Rust-first kernel, no_std and no_main; minimal architecture-specific assembly.
- Architecture: x86-64, one virtual CPU, UEFI; QEMU q35 machine, 512 MiB RAM
  (2 GiB since M5a).
- Kernel format: ELF for x86_64-unknown-none. Not a UEFI application pretending
  to be a kernel; execution must occur after firmware boot services have ended.
- Bootloader: bootloader crate 0.11.17 with matching bootloader_api 0.11.17 as the
  initial candidate. Confirm availability and compiler requirements in M0 before
  locking. A necessary version change must be documented with its reason.
- Firmware: OVMF/EDK II, separate code and per-run writable variable-store copy.
- Test execution: software CPU emulation as the portable default. Hardware
  acceleration can be optional, never required to reproduce the first boot.
- Logging: QEMU COM1 serial output, versioned startup and panic markers.
- Panic: abort semantics; report via serial, then halt. No unwinding or reboot.
- M1 memory ownership: consume the bootloader's documented BootInfo interface.
  Preserve bootloader mappings; no custom allocator or new page-table manager yet.
  (Superseded in M2; see "Resolved in M2".)
- Interrupts: keep disabled until deliberately implemented in M2. A halted M1
  guest is expected and must be terminated by the bounded host test harness.
- Host tooling: a small Cargo xtask program exposes build/run/verify commands.
  Do not require Bash on a Windows development host.

## Resolved in M0/M1 (2026-10-30)

Evidence for each item is in docs/verification.md.

- Toolchain: `nightly-2026-10-01` (rustc 1.101.0-nightly, 2026-09-30), pinned
  in rust-toolchain.toml with rust-src, llvm-tools, rustfmt, clippy and target
  x86_64-unknown-none. Nightly is needed only because bootloader 0.11.17's
  build script compiles its UEFI stage with `-Zbuild-std=core`. The kernel
  uses no unstable features and does not use build-std (x86_64-unknown-none
  ships a prebuilt `core`). The date was chosen as a recent nightly after the
  bootloader release (2026-07-27) and was confirmed by building and booting.
- Bootloader: bootloader 0.11.17 and bootloader_api 0.11.17, pinned with `=`.
  Only the `uefi` feature is enabled; the BIOS path is unused. Disk images are
  made with `bootloader::UefiBoot::create_disk_image` (GPT + FAT ESP).
- Host prerequisite: MSVC linker (already present). WSL and Bash are not
  required; all commands run from PowerShell.
- QEMU 11.1.0 installed with winget (user-approved system install). Firmware is
  the EDK II build bundled with it; see tools/firmware/README.md.
- QEMU machine: `-machine q35 -accel tcg -cpu qemu64 -smp 1 -m 512M`, firmware
  as two pflash drives (code read-only, per-run variable copy), `-nic none`
  (avoids PXE delays), `-display none`, `-monitor none`, `-no-reboot`.
- Serial: a small polled 16550 driver in the kernel (kernel/src/serial.rs)
  rather than an extra crate; port I/O asm is confined to kernel/src/cpu.rs.
  Waits for the UART are bounded.
- Normal and panic kernels are built with separate Cargo target directories
  (`target/kernel-normal`, `target/kernel-panic`) and image directories, and
  the panic kernel is selected by the `panic-test` Cargo feature. The kernel
  also prints `ZOO_OS INFO mode=…`, which the verifier checks, so a
  mixed-up image fails.
- The kernel prints an extra `ZOO_OS INFO` line with memory-map data from
  BootInfo. This is not part of the BUILD.md marker contract; it is evidence
  that kernel code ran with real boot data.
- Verifier: QEMU writes COM1 to a per-case file (`-chardev file`) and the
  harness polls it. Rules are in docs/verification.md. Negative cases: the
  panic image checked against normal expectations (must be rejected for a
  wrong marker) and the normal image with an extra marker that never appears
  (must be rejected by timeout, default 5 s).
- xtask parses its own arguments and uses only `bootloader` and `sha2` (for
  firmware checksums in doctor), to keep the dependency set small.

## Resolved in M2 (2026-10-30)

Full design: docs/m2-design.md. Evidence: docs/verification.md.

- Address-space ownership: the bootloader is configured with
  `dynamic_range_end = 0x0000_7FFF_FFFF_F000`, so all of its mappings stay in
  the lower half. Level-4 entries 256–511 belong to the kernel; the kernel
  checks they are empty at boot and panics otherwise.
- Kernel virtual layout: heap at `0xFFFF_C000_0000_1000` (1 MiB, fixed size)
  with unmapped guard pages on both sides; scratch test pages at
  `0xFFFF_C000_4000_0000`. These are internal kernel addresses, not an ABI.
- Physical memory: a bitmap frame allocator owns `Usable` frames at or above
  1 MiB. `Bootloader` and firmware regions are preserved, not reclaimed.
  Double frees and foreign frames are rejected with errors.
- Single owner, no globals: one `Memory` value holds the page mapper and frame
  allocator and is passed by `&mut`. Handlers never touch it.
- Heap: `linked_list_allocator` as the global allocator. This lifts M1's
  "no allocator" rule for the kernel, as M2 requires.
- Interrupt handlers use the unstable `abi_x86_interrupt` feature (via
  `x86_64`), so the kernel now depends on nightly itself, not only the
  bootloader build. Chosen over hand-written assembly entry stubs to keep
  assembly minimal. The kernel has no inline assembly of its own.
- Timer: legacy 8259 PIC + PIT at ~100 Hz. Local APIC/IOAPIC deferred.
- Double faults run on a dedicated 20 KiB IST stack so stack overflows are
  reported, not turned into a triple fault.
- Test modes: one Cargo feature per mode, mutually exclusive via
  `compile_error!`; one target dir and image per mode. The panic test now
  panics after all self-tests, with interrupts enabled.
- Crate version bumped to 0.2.0, so the BOOT marker reads `version=0.2.0`.
- Pins (exact): x86_64 0.15.5 (latest stable; 0.16 is a release candidate),
  pic8259 0.11.0, linked_list_allocator 0.10.6, spin 0.12.3 (0.10.0 was
  yanked).

## Interim console, pulled forward from M4 (2026-10-30)

The user chose to make the system visible and interactive before M3. Design:
docs/console-design.md. This is a kernel-mode debug console, not the desktop
or a user-space shell.

- Text drawn into the UEFI framebuffer with `noto-sans-mono-bitmap` 0.3.2
  (crate MIT, glyphs from Noto Sans Mono under the SIL OFL), regular 16 px,
  grey on black, 8 px margin, 18 px lines. The bootloader already depends on
  this font family.
- `kprintln!` writes to COM1 and the screen; markers stay on serial for the
  verifier. Panic and fault handlers use `try_lock` on the console so a fault
  while drawing cannot deadlock the report.
- PS/2 keyboard on IRQ1; the handler only queues bytes in a lock-free
  single-producer/single-consumer ring. Decoding uses `pc-keyboard` 0.9.0
  (scancode set 1, US layout) in the shell loop.
- The in-kernel shell owns the `Memory` value, so `mem` reports live state.
- Verification drives the real guest through QMP (`send-key`, `screendump`)
  and checks screenshots pixel for pixel against text re-rendered on the host
  with the same font. A wrong string is checked in the same run to prove the
  comparison can fail.
- `cargo xtask run` now opens a QEMU window (keyboard input goes there);
  `verify` stays headless.
- Version 0.3.0. Normal boots add `ZOO_OS SHELL ready` after `READY`.

## Owner decisions (2026-10-30)

- **Never test on the development machine** (absolute): ZooOS is never
  booted, installed or tested on the Windows PC this repository lives on,
  not even from USB, and its Windows installation, disks and boot settings
  are never touched. Only QEMU runs there (AGENTS.md).
- Real-hardware testing is viable on the owner's other machines, booted
  from USB.

- M5 uses a real instruct model, Qwen2.5-0.5B-Instruct (GGUF `q4_0`,
  Apache-2.0); the download was approved (tools/models/README.md).
  Software emulation is too slow for it; WHPX acceleration is available on
  the development host and may be used for inference, never required by
  verify.
- Opera/WoW stay as long-term goals, reached by building a POSIX layer
  milestone by milestone. GPU reference hardware and test bed undecided.
- The owner asked for autonomous progress: implement, verify, merge and
  continue without asking until told to stop.

## Desktop step 10: a cursor in Notes (2026-10-31)

Design: docs/d10-design.md.

- Left/Right arrow keys reach apps (`key:left`, `key:right`); Notes edits
  at a cursor. Version 0.38.0.

## Window server events (2026-11-04)

Version 0.108.0. Design: docs/events-design.md. Stage 4, second part.

- **Push, never wait:** subscribers get `event:<what>`; a full queue
  misses events (counted), so a stuck app cannot slow the desktop.
- **Focus changes go to subscribers only,** not to the screen snapshot's
  list.
- **Bella uses events for "it" in window commands only,** so file
  follow-ups keep their meaning.

## Sound: virtio-sound playback (2026-11-04)

Version 0.107.0. Design: docs/audio-design.md; the voice proposal is
docs/voice-plan.md (downloads await the owner).

- **virtio-sound first:** the same transport as the GPU driver, and QEMU
  records its output to a WAV file for an automatic pitch check. Intel
  HD Audio (real PCs) comes later.
- **Writes never wait for the speaker:** samples go into period buffers
  and the call returns what it took; waiting at playing speed would hold
  the scheduler lock for the length of the sound.
- **A permanent negative case** (`audio-silence`) keeps the pitch check
  honest.

## Draft model: measured, not used (2026-11-04)

Version 0.106.0. Design: docs/draft-design.md. Speculative decoding with
Qwen3-0.6B was slower on the CPU (0.42x at best): the draft's guesses were
kept 27–33% of the time, and a batched check costs about a step per token
on the CPU. The engine method, the loop and `draft-check` stay to rerun
at the GPU stage; Bella does not use them.

## Application actions (2026-11-04)

Version 0.105.0. Design: docs/actions-design.md. Bella roadmap Stage 4,
first part.

- **A static manifest in `zoo_abi`,** checked by the window server, so an
  app can only be asked what it declares, and delete-effect actions are
  refused until confirmation exists.
- **The window server routes and remembers who asked;** the app that did
  the work writes the sentence Bella shows.
- **Two actions first:** Notes `write` (dictation) and Network `fetch`.
  Files needs none: `open` shows folders.
- **The text and address come from the request,** as for tiling.

## More than one monitor (2026-11-04)

Version 0.104.0. Design: docs/monitors-design.md.

- **One wide screen, a scanout per monitor:** the window server keeps one
  desktop; the kernel shows a part of it on each monitor. No per-monitor
  surfaces to keep in step.
- **Built on virtio-gpu now, real monitors later:** UEFI gives one
  framebuffer, so real hardware has one monitor until a GPU driver
  reports more through the same `DisplayInfo::monitors`.
- **The menu bar and dock stay on monitor 1.**
- **The monitor is read from the request, not added to the model's
  prompt:** no change to its choices (`assist-check` 32 of 34, 0 of 8).
  A request naming a monitor and an app is always a window placement, so
  "move files to the second screen" cannot move a file.

## Requests of several steps (2026-11-04)

Version 0.103.0. Design: docs/multi-step-design.md.

- **Steps are split by rules, each chosen by the model on its own.** One
  model call per step, as for a single request; no plan for the model to
  write and get wrong.
- **"and" splits only before a step word,** so "split Notes and Files"
  and "search for flour and sugar" stay one step.
- **"it" names the apps of the step before;** otherwise the follow-up
  context does as before.
- **The model's conversation note waits for the last step:** it cost
  about 1.2 s between steps.
- **App names come from the request for every app command,** and an
  `open` naming two apps launches them.

## Tiling (2026-11-03)

Version 0.102.0. Design: docs/tiling-design.md. The owner's request, with
free wording and several windows of one app.

- **The model chooses `split`; the apps come from the request's words.**
  When the model wrote them, it used its prompt's order and invented apps
  for "these 2 windows". Reading them from the request keeps the user's
  order and sides, and the model writes nothing after `split`.
- **No app named: the two front windows** other than Bella's.
- **The same app twice: two of its windows,** opening one if needed.
- **`info` is described as "this computer's storage and memory":** the
  new command line had tipped "What time is it?" to `info` (1 of 8 acted
  on); now 0 of 8 again.

## Bella's models on the USB stick (2026-11-03)

Version 0.101.0. Design: docs/usb-model-design.md. Hardening before the
first real-PC test.

- **xtask builds the FAT partition and GPT disk itself** from the files
  `bootloader` exports. Its own builder leaves no room for the FAT
  tables, and failed above about 600 MB. Rather than patch or fork the
  bootloader crate, the layout (about 100 lines, the same as its
  `gpt.rs`) moved to xtask, with the same pinned `fatfs` and `gpt`.
- **The stick carries both models in its boot image,** mapped in place.
  A real PC has no model disk until ZooOS has a USB storage driver.
- **`verify` keeps the model disk for the Bella cases;** only
  `usb-model` checks the stick's image.
- **Process exit was measured, not changed:** at most 20 Mcycles of lock
  for a 1.2 GB process.

## Window actions (2026-11-03)

Version 0.100.0. Design: docs/window-actions-design.md. First step of the
owner's voice-control goal.

- **The window server does the action and says what it did**
  (`window-done:`), so Bella never reports an action that did not happen.
- **`maximize` opens the app first,** so "open Notes and maximize it" is
  one command, not a request of two steps.
- **The same functions as the traffic lights and the dock,** so the
  animations and the dock stay right.
- **0.5B does not get the new words:** listed in its prompt, they made it
  act on 4 of 8 requests that have no command (1 before). Qwen3 got all
  four new requests right and still acts on none of the 8.

## Keys under load (2026-11-03)

Version 0.99.0. Design: docs/keys-design.md.

- **`mem_alloc` maps at most 1 MiB per pass** and runs the call again
  with the lock released, instead of mapping a 1.2 GB heap in one hold.
  Measured: that hold stalled all 8 processors for seconds and QEMU's
  keyboard queue dropped keys. Programs see one call, as before.
- **8 MiB passes were not enough** (4 of 5 runs passed): two services
  allocating at once took turns with the lock.
- **A ticket lock was tried and not kept:** fairness does not help when
  one hold lasts seconds.
- **PeacockHandler holds up to 256 keys per busy app** and delivers them
  in order, instead of dropping a key when the app's queue is full.

## Qwen3-1.7B (4 bits) is Bella's default model (2026-11-03)

Version 0.98.0. **The owner's choice**, after the measurements on 8
processors: Qwen3 `Q4_0` small talk 0.78 s and an answer from a fact
1.32 s, with fuller summaries than 0.5B (docs/smp-design.md,
docs/qwen3-design.md). `--bella 0.5b` keeps the old default.

- **The default is named once** (`infer::DEFAULT_MODEL`). Whether a model
  fits the boot image, and whether the guest needs 4 GiB, now follow the
  model's size (`BOOT_IMAGE_MODEL_MAX`, 600 MB), not "is it the default".
- **Qwen3's 1.13 GB file does not fit the boot image,** so Bella reads it
  from the model disk. In consequence:
  - `inference` and `bella-model` (the model in the boot image) are
    skipped by default; they still run with `--bella 0.5b`;
  - `run --model`, `model-image` and `usb-image` stop with a clear
    message and point to `--bella 0.5b`;
  - the USB stick has no model disk yet, so real hardware boots with
    0.5B until it gets one.
- **`infer-check` checks with 0.5B by default:** the NumPy reference
  implements Qwen2 only.
- **The 4-bit file is made, not downloaded** (`cargo xtask requant
  --bella qwen3-1.7b`). A missing file says so.

## More than one processor, steps 2 and 3 (2026-11-03)

Version 0.97.0. Design: docs/smp-design.md.

- **One kernel lock,** with each processor's current task swapped in,
  instead of rewriting the scheduler. Programs run in parallel; kernel
  code runs on one processor at a time.
- **A task stays in hand until its old processor is off its stack**
  (`smp::after_switch` from `trap_common`). This was the first crash
  found.
- **Linux programs stay on the boot processor.** Their `syscall` entry has
  one scratch stack variable.
- **The model's matrix rows split across a worker pool.** Results are
  identical, because the dot products are the same.
- **Each program may have 1 MiB of messages queued.** The 4-processor
  suite showed the shared heap reserve alone let one program starve the
  services (docs/heap-design.md).
- **`run` uses 8 processors.** `verify` keeps 1 unless `--cpus`.
- **Measured:** with 8 processors, 0.5B answers every case in under a
  second, and Qwen3 small talk takes 0.78 s.

## More than one processor, step 1 (2026-11-03)

Version 0.96.0. Design: docs/smp-design.md. The owner's 16-core PC ran
ZooOS on one core; answers in under a second need the others.

- **In three steps, each mergeable:** start and park the processors; run
  tasks on them under one kernel lock; split the model's work across
  them.
- **The trampoline is the only new assembly,** in `global_asm!` (about 50
  instructions): real mode to long mode, with temporary tables that map
  the trampoline's page to itself and copy the kernel's upper half.
- **Every existing case still runs with one processor;** `smp-` cases get
  4.

## 8-bit activations (2026-11-03)

Version 0.95.0. Design: docs/int8-design.md. The owner's goal is answers
in under a second; they chose this on 2026-11-03 ("do everything").

- **`Q4_0` and `Q8_0` rows are dotted with 8-bit activations** in
  integers, as llama.cpp does: about 1.3× faster.
- **Kept although `infer-check` reports `FAIL`.** It failed on one point
  of the bar set before measuring: a 5th-place near-tie swaps on 2 of 5
  prompts. Every output token, every command choice and the embedding
  scores are unchanged. `--f32-activations` turns it off.

## Bella answers from your files; a file index by meaning (2026-11-03)

Version 0.94.0. Designs: docs/lookup-design.md, docs/vectors-design.md,
docs/batch-design.md.

- **The owner approved an embedding model.** Qwen3-Embedding-0.6B was
  chosen because it runs on the engine we have (Qwen3, last-token
  pooling), is Apache-2.0, and scores among the best small models. A tiny
  BERT model would be faster, but would need a new engine.
- **Model disk version 2:** up to 4 models; model 0 stays the chat model,
  so nothing that reads model 0 changed.
- **MagpieHandler at a new idle priority:** the first index takes about
  2 minutes for 152 lines, so it must never compete with the desktop or
  the chat model.
- **Word lookup stays** as the fallback while the index is building, and
  when no embedding model is installed.
- **Prompts are read in batches of 16:** results identical, 1.4× faster
  on the host. Reading a prompt is limited by arithmetic, not by memory,
  so the next speed levers are more cores and 8-bit activations.

## Qwen3-1.7B in 4 bits; B14 closed as measured (2026-11-03)

- **`cargo xtask requant`** turns the official `Q8_0` file into `Q4_0`.
  The token embedding stays `Q8_0`, because it is also the output layer.
  Deterministic. Qwen3 then runs at about Qwen2.5-1.5B's speed, with
  better summaries and no wrong actions in `assist-check`
  (docs/qwen3-design.md). The default stays 0.5B until the owner chooses.
- **B14's key part is not changed:** broker calls measured 2–10 ms, and
  the 10 s deadline already bounds a hung broker (docs/stage2-design.md
  §4).

## Replies as they are written; Qwen3 for evaluation (2026-11-03)

Version 0.93.0. Designs: docs/stream-design.md, docs/qwen3-design.md.

- **Owner's choice:** an instant line before slow answers, and replies
  shown word by word, rather than only faster models.
- **The reply so far travels as progress** (`part <text>`). It is
  dropped when Bella's queue is full, and the next one carries all of it.
- **Qwen3-1.7B downloaded with the owner's approval** and supported in
  the engine (QK-norm, no biases, thinking off). It stays an evaluation
  model; the default stays 0.5B until the owner chooses.

## Screen snapshot wording for both models (2026-11-03)

Version 0.92.0. A window's title is written "Notes showing zoo.rs", not
"Notes (zoo.rs)": with the old form the 1.5B model answered "the Notes
file". Both models now name the file (docs/context-design.md).

## Model cases run with the 1.5B model from the model disk (2026-11-03)

`cargo xtask verify --bella 1.5b` used to skip every Bella model case,
because the 1.5B model does not fit the boot image's FAT partition. Those
cases now boot the plain image with the model on the model disk, as
`run` does. Only `inference` and `bella-model`, which check the model in
the boot image, are still skipped. The default model is unchanged: it
stays in the boot image. The default stays 0.5B: the comparison in
docs/verification.md is the evidence for the owner's choice.

## Bella saves a summary (2026-11-03)

Version 0.91.0. Design: docs/save-design.md.

- **"Save that"** after a summary writes `<stem>-summary.txt` beside the
  file, through a new broker tool `write` (audited, at most 900 bytes).
- **`write` never replaces a file.** Overwriting is deleting the old
  contents, and the owner's rule is to confirm deletes; refusing is
  simpler than asking.

## Bella summarises a file (2026-11-03)

Version 0.90.0. Design: docs/summary-design.md. The first Stage 5
workflow.

- **A call of its own:** `Talk::summarise` writes past the conversation
  without keeping anything, so a file's text (and anything it tries to
  instruct) never stays in the model's context. Bella notes the summary,
  not the text.
- **Capped at 900 bytes**, read through the broker like any file, marked
  as data in the prompt, with the request after the text.

## The kernel heap under pressure; Bella's audit trail (2026-11-03)

Version 0.89.0. Designs: docs/heap-design.md, docs/audit-design.md.
Stage 2 items B13 and "audit lines".

- **A 1 MiB heap reserve** that program requests (`CHANNEL_CREATE`,
  `SEND`, `SEND_HANDLE`, `SPAWN`) may not use: they get `NOMEM`, and the
  kernel keeps working. Chosen over per-process quotas, which need every
  allocation charged and moved with messages; the reserve protects the
  property that matters (no panic).
- **No channel ends parked where no close reaches them:** `SEND_HANDLE`
  refuses an end over its own channel (`INVAL`) and an end whose channel
  carries a handle (`BUSY`). This rules out cycles, which leaked.
- **The audit trail:** OtterHandler logs every request with its outcome,
  and writes each change that worked to `.bella-audit` (dated, 16 KiB
  cap). Reads stay out of the file. "What did you do?" lists the last 8.

## Bella sees the screen (2026-11-03)

Version 0.88.0. Design: docs/context-design.md. The first piece of Stage 4.

- **Pull, not subscribe:** for "what am I working on?" and the like,
  Bella sends `context?`. PeacockHandler answers with the focused window,
  the open windows (titles where they add something) and the last six
  things that happened.
- **The snapshot goes to the reply model** as "On my screen: …". Window
  titles and paths reach the model; window contents do not.

## Bella remembers facts (2026-11-03)

Version 0.87.0. Design: docs/facts-design.md.

- **"remember that X"** keeps X in `.bella-facts` in home (at most 8 KiB).
  **"what do you remember?"** lists the facts. **"forget X"** asks first.
- **Facts sharing words with a question** go with it into Bella's reply in
  words. A question tied to a fact by two or more words is answered in
  words directly.
- **Matching is by shared words for now.** A vector store needs an
  embedding model, a download for the owner to approve.

## Broad access, confirm deletes (2026-11-03)

Version 0.86.0. Design: docs/access-design.md. The owner chose "broad,
confirm deletes".

- **New commands:** `read`, `open` (Files or Notes), `move` (undoable),
  `trash` (asks first; into the Trash, so Put Back works), `launch` and
  `info`.
- **Names are resolved against what exists:** letter case, then a single
  match by name or stem anywhere in home. Only for commands about
  something that already exists; never for `create` or a move's target.
- **The model chooses among ten commands plus `none`,** each described in
  a few words. 0.5B: 17 of 23 right, 2 of 8 acted on (the new bar). 1.5B:
  19 of 23, 5 of 8, all `launch` of apps that do not exist.
- **Read file contents can reach the model** through the conversation.
  Accepted under the owner's rule: destructive commands ask.

## Stage 3: XSAVE and AVX2 (2026-11-03)

Version 0.85.0. Design: docs/stage3-design.md.

- **The kernel switches tasks with `xsave64`/`xrstor64`** when the CPU has
  XSAVE, with XCR0 = x87 | SSE | AVX, in a 1,024-byte area. Otherwise it
  keeps FXSAVE, with AVX off. Signal frames keep the 512-byte FXSAVE
  part.
- **`zoo_infer` chooses an AVX2 and FMA dot product at run time**
  (`cpu::avx2()`: CPUID plus XGETBV). The crate goes from `forbid` to
  `deny(unsafe_code)`, with `allow` only on `avx2`, `cpu`, and the call
  that chooses them.
- **WHPX guests get `qemu64,+xsave,+avx,+avx2,+fma,+f16c`;** `-cpu host`
  faults in OVMF. TCG keeps `qemu64`.
- **Measured in the guest:** requests about twice as fast (0.5B: 1.08 →
  0.52 s), tokens unchanged. **0.5B stays the default;** 1.5B takes about
  2 s per request.

## Bella keeps the conversation (2026-11-03)

Version 0.84.0. Design: docs/talk-design.md §6.

- **Replies in words continue one conversation** in the model's cache (768
  tokens, then it starts over).
- **Command exchanges are noted into it,** including the names in their
  results.
- **Follow-ups** ("is that it?") send the last exchange with the command
  choice. A follow-up's empty command is answered in words.
- **`.` and `~` mean the home folder.**

## Bella talks; Qwen2.5-1.5B as a choice (2026-11-02)

Version 0.83.0. Design: docs/talk-design.md.

- **A line is a typed command** only if it has no sentence words, so
  "search for my music" goes to the model. Small talk is answered in
  words. The rules are in `zoo_abi::bella`, with host tests.
- **Bella answers in words** (`zoo_infer::talk`, its own system prompt)
  for small talk and when no command fits. The reply is shown, never run.
- **`--bella 1.5b`** chooses Qwen2.5-1.5B-Instruct (Apache-2.0, pinned;
  owner-approved download). The engine gains `Q6_K`. `HEAP_LIMIT` goes to
  4 GiB and the guest gets 4 GiB with it. On the model disk only.
  - **Measured:** 11 of 14 against 10 of 14 for 0.5B, but about 3.5
    times slower (request 3.7 s in the guest).
  - **0.5B stays the default** until Stage 3's speed-up.
- **The owner's direction on access: "broad, confirm deletes".** Bella
  will reach the whole home folder, file contents and apps through the
  broker; only destructive actions ask. Not built yet.

## Stage 2: Bella's helpers start again (2026-11-02)

Version 0.82.0. Design: docs/stage2-design.md §4.

- **Bella keeps her home handle** and gives each file broker a copy, so a
  broker that ends can be replaced.
- **ElephantHandler and OtterHandler are started again** after a back-off
  (0.5 s, doubling to 16 s). The model is warmed up again.
- **Broker answers have a 10 s deadline** (part of B14).
- **The shell's `kill <program> [after <s>]`,** for tests.

## Stage 2: the model on its own disk (2026-11-02)

Version 0.81.0. Design: docs/stage2-design.md §3.

- **A read-only model disk** (`build/model-disk.img`): a `ZOOMODL1` header
  block, then the model. Chosen over the data disk to keep the model
  apart from the owner's files and safe from guest writes.
- **New system calls `MODEL_SIZE` (76) and `MODEL_READ` (77).** The
  kernel tells the two virtio disks apart by the header.
- **`cargo xtask run`** boots the small image plus the model disk: the
  desktop is up 1.72 s sooner (1,210 ms against 2,930 ms). The model is
  read in 341 ms in the background.
- **`usb-image` keeps the model built in** until there is a USB storage
  driver.
- **B10 (polled disk requests under the scheduler lock):** measured at
  about 52 µs per 64 KiB request, and left for Stage 7.

## Stage 2: the warm-up (2026-11-02)

Version 0.80.0. Design: docs/stage2-design.md §2.

- **Bella starts the model service as it opens** (when the model is
  installed) and asks it to read the system prompt in the background.
- **The first request now takes about 1 s,** like later ones; it took 6 s.
- **A request during the warm-up** is kept and answered after it, with
  progress shown; `cancel` drops it.

## Stage 2: background programs (2026-11-02)

Version 0.79.0. Design: docs/stage2-design.md (B12).

- **New system call `SET_BACKGROUND` (75).** A background program runs
  only when no other program is ready. After 1 s without a turn it gets
  one, so it cannot starve.
- **Input that wakes a task preempts a background program at once.**
- **ElephantHandler marks itself background.** Measured: input wait
  during inference dropped from 4.7 ms to 0.25 ms on average; requests
  are 2-5% slower.
- **One flag, not priority levels:** inference is the only long
  computation today.

## Stage 1: "none of these" and timing lines (2026-11-02)

Version 0.78.0. Design: docs/stage1-design.md.

- **A fifth tool word, `none`**, and a prompt line asking for it when the
  request is not about files. Measured on 22 requests:
  - before: 12 of 14 right, 8 of 8 out-of-scope requests acted on;
  - after: 10 of 14 right, 2 of 8 acted on.
  The two losses are `list` answered as `search`.
- **`assist-check`** fails below that bar (at least 10 right, at most 2
  acted on).
- **Timing lines** in ElephantHandler (set-up, reading, writing, heap)
  and Bella (Enter to answer). `zoo_rt::now_us()` uses the cycle counter;
  the kernel clock is 10 ms ticks.

## The assistant is named Bella (2026-11-02)

Version 0.77.0. The owner's decision: Bella, not Jarvis, everywhere.

- **The app** is Bella: the dock label, the window title, the menu bar, the
  Applications folder. The program is `bella` (was `assistant`).
- **The transcript** reads `Bella:` and `Bella (remembered):`, and log
  lines start with `Bella:`.
- **The engine's prompt wrapper** is `zoo_infer::assist::Bella`. The
  model's prompt never named the assistant, so answers do not change.
- **Kept across the rename:**
  - A dock saved with `assistant` still pins Bella.
  - OtterHandler moves `.assistant-memory` to `.bella-memory` the first
    time it starts.
- **Renamed test cases:** `bella`, `bella-memory`, `bella-model`,
  `bella-live` and `bella-any-case`, formerly `assistant…`.
- **Not renamed:** design documents written before 0.77.0 keep "the
  Assistant", as the record of those steps.

## Stage 0: restarts and the filesystem doc (2026-11-02)

Version 0.76.0. Design: docs/s0-design.md (B5, B15).

- **Restart counts reset** after a service runs 30 s without failing.
- **A desktop session gets a line from the resident supervisor**
  (`TAG_SESSION_LINE`, 6), passed by the kernel with its home folder.
  After BadgerHandler restarts, a fresh home folder arrives on it and the
  desktop reloads: open windows close, files are kept. Chosen over
  handing every app a new handle, which would need a new message in
  every app.
- **`restart <svc> after <seconds>`** in the shell schedules a restart (for
  tests while the desktop has the keyboard).
- **docs/m5c1-design.md** gains a current-state section.

## Stage 0: the memory (2026-11-02)

Version 0.75.0. Design: docs/s0-design.md (B3, B4, B8).

- Only `search`, `list`, `inspect` and `create` are learned or replayed.
  Anything else in the memory file is ignored and logged, because any
  program with home access can write that file.
- A search with no matches finishes with `none`: it is shown but never
  learned.
- A recall is not learned again, and leaves the request open for a typed
  correction.
- A pair is recalled only if the names its command took from the
  remembered request also appear in the new one.
- The shell's `put` writes `	` as a tab. The test harness gains
  `NeverSeen`, a check that a line never appeared.

## The Bella roadmap and Stage 0 (2026-11-02)

The owner's direction: the assistant is named Bella. docs/bella-roadmap.md
and docs/bella-audit.md set the order of work. The model is not fixed to
Qwen; its weights must be open source. Version 0.74.0 is Stage 0's
filesystem part (docs/s0-design.md):

- **B1:** Notes edits files up to 1 MiB, read whole. A larger file, or one
  read only in part, opens read-only and is never saved.
- **B2:** folder channels carry their folder's generation; a removed
  folder's channel answers `NotFound`.
- **B6:** new system call `SHM_SIZE` (73). BadgerHandler only uses a
  client buffer of at least `BULK_SIZE`.
- **B7:** new system call `CHANNEL_PEER` (74). One process may hold at
  most 31 of the 62 folder channels. Two processes acting together can
  still take them all; per-process quotas set by the supervisor would
  fix that, and are not done.

## Assistant step 2b: names in any letter case (2026-11-02)

Version 0.73.1.

- Found by the owner testing: "where is my music" → the model chose
  `list music`, and the folder is `Music`, so it failed with "not found".
- OtterHandler's `list`, `inspect`, `plan` and `create` now take each
  name that exists only in another letter case as the name on disk, as
  macOS does (``OtterHandler: `music` is `Music` ``). Names that match
  nothing, or more than one entry, stay as written. The disk itself stays
  case-sensitive; only the Assistant's tools match loosely.

## Assistant step 3: the model in the everyday system (2026-11-02)

Design: docs/a3-design.md. Version 0.73.0.

- The owner asked for the model to be part of the OS; `run` and
  `usb-image` now include it when it is in `tools/models/`, `--no-model`
  leaves it out.
- Measured cost: +1.85 s of boot under WHPX (the boot loader reads 411 MB
  more). Accepted for now because the owner asked for it; the follow-up is
  loading the model from the data disk after boot, which removes the cost.

## UI step 3h: Rename and the Trash (2026-11-02)

Design: docs/u3h-design.md. Version 0.72.0.

- The Trash is a hidden `.Trash` folder in home; moving there is a rename,
  so nothing Files does deletes data. Emptying the Trash is left for a
  later step.

## UI step 2h: the launch bounce (2026-11-02)

Design: docs/u2h-design.md. Version 0.71.0 (with U4b).

- Two hops over 640 ms when an app is asked to open; not for the windows
  the desktop opens at start-up. The dock stays up while an icon hops.

## UI step 4b: kept appearance, word wrap (2026-11-02)

Design: docs/u4b-design.md. Version 0.71.0.

- The appearance lives in `.appearance` in the home folder, next to
  `.dock`; only the user's switch writes it.
- Notes breaks lines after the last space within 72 characters.

## UI step 4a: menus that open (2026-11-02)

Design: docs/u4a-design.md. Version 0.70.0.

- The menu bar's menus reuse the context-menu machinery (one `Menu`
  type), so they look and close the same way.
- Ctrl stands in for the Mac's Command key: Ctrl+N/W/M are the window
  server's; other Ctrl+letters stay with apps (Notes' Ctrl+S, Files'
  Ctrl+O).
- Edit's items are shown dim until there is a clipboard, rather than
  hidden, so the menu bar reads as the owner expects from macOS.

## UI step 2g: dock magnification (2026-11-02)

Design: docs/u2g-design.md. Version 0.69.0.

- On by default (the Mac look the owner asked for), only while the
  pointer is over the icons, so revealing the dock at the bottom edge does
  not magnify.
- Anchored at the pointer, clicks matched to resting positions: the same
  click picks the same app as before, so existing behaviour and tests hold.
- Icons cached at the magnified size and scaled down (bilinear), never up.

## Network step 3: the Network app and DNS (2026-11-02)

Design: docs/n3-design.md. Version 0.68.0.

- Network is a fifth application, in Applications but not in the dock at
  first (so the dock the owner knows is unchanged); window slots 8 → 9.
- DNS messages are pure functions in `zoo-abi`, unit-tested on the host;
  guest tests stay hermetic (no DNS server on the isolated network).
- Plain HTTP only for now; TLS is a separate, larger step.

## Network steps 1 and 2: a network device, IPv4, TCP (2026-11-02)

Designs: docs/n1-design.md, docs/n2-design.md. Version 0.67.0.

- virtio-net through the legacy interface, polled, as the block driver:
  the smallest working driver. Interrupts come with a network service.
- Raw frames go to one process at a time (`NET_ATTACH`), like the
  display; the stack (Ethernet, ARP, IPv4, ICMP, UDP, DHCP, a TCP client)
  is in user space (`zoo_rt::net`). A supervised network service with
  sockets is the next step, and is when other programs stop seeing raw
  frames.
- Tests are hermetic: `net-` cases use QEMU's user network with
  `restrict=on` (no host or internet access); the TCP test reaches a
  server the harness runs on 127.0.0.1 through `guestfwd`. `run` gets the
  unrestricted network.
- Test program names: `tern-test` and `puffin-test` were already C test
  programs, so the network checks are SnowGooseTest and GyrfalconTest.

## Assistant step 2: the Assistant learns from use (2026-11-02)

Design: docs/a2-design.md. Version 0.66.0.

- Learning is a memory of request → command pairs in the home folder
  (`.assistant-memory`), not a change to the model: the base model stays
  frozen, as the owner asked, and the memory is visible and editable.
- It learns from answers that worked and from corrections (a command
  typed after a request it could not answer well).
- A request at least 75 % alike (shared words, filler dropped) is
  answered from memory without the model; otherwise the model is asked.
- The memory lives in OtterHandler, which already holds the home folder;
  the model service still has no file access.

## Assistant step 1: a faster model engine (2026-11-02)

Design: docs/a1-design.md. Version 0.65.0.

- Vector (`core::simd`) dot products in float, scores only for tokens
  that are read, rotary frequencies precomputed: 2.0–2.7× faster in the
  guest, with answers unchanged.
- 8-bit integer activations (llama.cpp's way) would be about 25 % faster
  again, but fail the agreed 0.02 logit check against the reference (0.21).
  Not taken without the owner's say.

## UI steps 3f and 3g: Finder-style Files, the editor, icons (2026-11-02)

Designs: docs/u3f-design.md, docs/u3g-design.md. Version 0.64.0.

- Apps style their lines with a few control characters (colour roles,
  icons, a caret, a right column) instead of drawing: the window server
  still draws everything, in the system's colours and both appearances.
- Clicks reach apps (`click:`, `dclick:`, `sclick:`, `rclick:none`), and
  windows can have a sidebar drawn with their frame.
- Notes became the text editor: it opens files Files sends it and
  colours program text by extension (Xcode's colours). Inter stays the
  font: a monospaced font would be a new download, which needs the
  owner's approval.
- Files keeps its title and its location line (`Home/Documents`), so the
  window is still found by the same name; folders lose their `/`.
- Icons are drawn from shape descriptions at the screen's scale (no image
  files); dock icons are cached as premultiplied images because the dock
  is drawn in every frame of its slide.
- A sample program, `Documents/zoo.rs`, is in the seed home folder to show
  the colours; the boot self-check now expects 3 entries in Documents.

## UI step 3: windows, dock menus, Applications (2026-11-01)

Design: docs/u3-design.md.

- Window slots are separate from applications (8 slots, 4 apps); each
  window is its own process. Version 0.63.0.
- Apps get context menus through a small text protocol (`rclick:`,
  `menu:`, `menu-choose:`, `dock-add:`, `launch:`) rather than drawing
  menus themselves, so every menu looks the same and closes the same way.
- Files' Applications folder is virtual (the list of apps), placed last
  in Home so existing folder positions are unchanged.
- The dock's kept apps are saved in `.dock` in the home folder; no file
  means every app is kept.
- The owner reordered the overnight work (2026-11-01 23:15): the
  assistant's learning and speed pass comes before networking.

## UI step 2f: the ZooOS mark (2026-11-01)

Design: docs/u2f-design.md.

- The owner chose logo A, the soft paw, from four directions shown as
  images. It is drawn from a coverage mask computed at start-up (vector
  shape, 16 samples a pixel), at 1x and 2x.
- The assistant work (speed, the model in the everyday image, learning)
  comes after the UI polish pass, as the owner confirmed.
- Version 0.62.0.

## UI step 2e: the dock hides itself (2026-11-01)

Design: docs/u2e-design.md.

- At the owner's choice, the dock is auto-hidden. It slides in at the
  bottom edge and out 400 ms after the pointer leaves, and it shows during
  a genie.
- Verify cases reveal the dock at the bottom edge before clicking an icon,
  as a person would.
- Version 0.61.0.

## UI step 2d follow-up: the genie (2026-11-01, 0.60.0)

- At the owner's request ("doesn't look as much like mac"), minimise and
  restore are a genie warp: S-curved edges from the window to the dock
  icon, pinch, then slide. See docs/u2d-design.md §6.
- Shift held runs new animations at a tenth of the speed, as on a Mac.
  The `genie` case uses it to check the warp's shape.

## UI step 2d: window animations (2026-11-01)

Design: docs/u2d-design.md.

- An animation clock from the time-stamp counter, calibrated against the
  tick at start. Frames come at the U2a pacing rate (about 100 a second)
  while anything animates.
- Open, close, minimise and restore animate a window's rectangle and
  opacity, drawing its layer scaled bilinearly. Closing and minimising
  windows draw from a snapshot.
- The frame report now includes the longest frame.
- Version 0.59.0.

## UI step 2c: the pointer moves with a dragged window (2026-11-01)

Design: docs/u2c-design.md.

- During a drag or resize the hardware cursor is hidden and the arrow is
  drawn in the frame with the window, so they move together. The owner
  approved this after trying it. At other times the hardware cursor stays,
  for instant pointer movement.
- Version 0.58.1.

## UI step 2b: the monitor's own resolution, and 2x for 4K (2026-11-01)

Design: docs/u2b-design.md.

- The owner's main monitor measured 2560×1440, not 4K. `xtask run` passes
  the host monitor's size, when larger than 1920×1080, through QEMU
  fw_cfg (`opt/zooos/display`), which the kernel reads only after finding
  QEMU's virtio GPU.
- The window server draws at 2x from 3200×1800 up, with logical layout and
  device-pixel drawing (2x Inter faces in the atlas). The logical limit
  rose to 2560×1600, so a 1440p monitor is used whole at 1x.
- Version 0.58.0.

## UI step 2a follow-up: pacing and the pointer (2026-11-01, 0.57.1)

- Frames are paced to at most one a timer tick (100 a second) during
  continuous motion. Measured: QEMU's display, not the window server, fell
  behind on fast drags. Draining all queued input before a frame.
- A new, anti-aliased macOS-style arrow pointer with an outline and
  shadow, used as both the hardware cursor and the software pointer.
- `ZOO_VERIFY_WINDOW=1` shows QEMU's window during verify, for measuring
  its display cost.

## UI step 2a: a compositor in the window server (2026-11-01)

Design: docs/u2-compositor-design.md.

- Windows are cached as layers with alpha, built by drawing each twice
  (over black and over white), so the existing drawing code stays the
  only drawing code.
- Moves only composite. Look changes rebuild the layer. App line changes
  redraw just those lines in place. Live resizes draw directly.
- Overlapping damage merges. On the virtio GPU the window server draws
  straight into display memory.
- Frame pacing was not added: every measured frame is under 1 ms, so
  pacing would only add delay.
- The verify `redraw` case now drags in four moves and ends with a
  different focus than it started with. The new `redraw-content` case
  checks in-place content updates. Each was needed to catch a control the
  old case missed.
- Version 0.57.0.

## GPU step 1: virtio-gpu display and a hardware cursor (2026-11-01)

Design: docs/gpu1-design.md.

- After U1c the owner still felt lag in QEMU and asked for the
  engineer's call. The remaining lag was QEMU copying a plain framebuffer
  to its window on a timer. virtio-gpu replaces that with explicit
  flushes and a cursor plane the host draws.
- `virtio-vga` is chosen, not plain `virtio-gpu`: firmware, bootloader and
  console keep the VGA side unchanged. A device reset on display release
  returns to it.
- The screen is one contiguous RAM block mapped into the display owner.
  Requests are synchronous and polled. Measured: a full repaint with its
  flush costs the same as on VGA.
- `xtask run` uses `virtio-vga` with the tablet. Only `gpu-` cases use it
  in verify; the rest stay on standard VGA, the real-hardware path.
- ABI: `DisplayInfo::flags`, `DISPLAY_FLUSH` (66), `CURSOR_SET` (67),
  `CURSOR_MOVE` (68).
- `xtask run` previously ignored `--accel` (every interactive session ran
  under TCG). It is fixed, with `run --model` added.
- Version 0.56.0.

## UI step 1c: USB input by interrupt, and a tablet for QEMU (2026-11-01)

Design: docs/u1c-design.md.

- The owner still felt a slight lag after U1b and asked for an engineer's
  call. The order chosen:
  1. USB input by interrupt, which matters on real hardware and which the
     tablet needs;
  2. the tablet, for how QEMU feels;
  3. acceleration, deferred until a real mouse can be tried.
- The xHCI interrupt goes through MSI-X, else MSI, to the local APIC
  (vector 0x50). The tick still polls as a fallback.
- `cargo xtask run` uses a USB tablet. The verify cases keep their PS/2
  and USB mice, so the relative path stays tested.
- HID report descriptors are parsed for pointers without a boot protocol
  (`kernel/src/hid.rs`). Boot-protocol keyboards and mice are unchanged.
- ABI: `MOUSE_ABSOLUTE` (0x80) in `MouseEvent::buttons`, coordinates
  0..=`MOUSE_ABS_MAX` (0x7FFF).
- Version 0.55.0.

## UI step 1b: input without waiting for the tick (2026-11-01)

Design: docs/u1b-design.md.

- The owner found U1 jittery and rigid. The cause, measured: mouse events
  waited for the next 10 ms tick before the window server saw them (mean
  about 5 ms, longest about 10 ms).
- Keyboard and mouse interrupts now enter the task-switching trap path and
  wake a display owner blocked in the new `INPUT_WAIT` (64). They switch
  only away from the idle kernel task; a busy program is not preempted.
- `INPUT_LATENCY` (65) is a measurement aid. The `frames` case checks
  the mean wait against a share of a tick, so it holds without an
  accelerator too.
- The timer stays at 100 Hz. An absolute USB tablet for QEMU and
  interrupt-driven USB input are left for later.
- Version 0.54.1.

## UI step 1: a macOS-style desktop (2026-11-01)

Design: docs/u1-design.md.

- Owner's choices: Inter (download approved, 33.7 MB release zip, three
  TTFs used; tools/fonts/README.md), light and dark with a switch, a much
  higher resolution with uncramped windows.
- Text is rendered at build time (`fontdue` =0.9.3 in xtask only) into an
  initrd atlas; the atlas format and layout code are shared in `zoo-abi`,
  so screenshot checks reproduce the screen exactly.
- QEMU's display prefers 1920x1080 through its EDID; the bootloader's
  minimum-size option picked 2560x1600 instead.
- Desktop geometry and both palettes live in `zoo_abi::ui`, shared with
  the tests.
- The bottom status line became notification banners.
- The dock is always shown.
- The menu bar is opaque, which keeps its text checkable exactly; the
  gradient barely showed through.
- Menus are labels until U4.
- Version 0.54.0.

## Linux step 10d: Wine's prefix in the home folder (2026-11-01)

Design: docs/linux10d-design.md.

- Names: any UTF-8 without `/` or NUL, up to 255 bytes.
- `MAX_MESSAGE` is 1 KiB and the file protocol's path length is 2
  bytes.
- Linux paths go up to 4,096 bytes, copied to the heap.
- Home-folder links are stored by BadgerHandler and followed by the
  kernel from a copy read once (`ListLinks`) and kept current by its own
  calls. ZooOS programs do not make links yet.
- `mmap` of a home file is a private copy filled across re-runs of the
  call; a writable `MAP_SHARED` mapping of one is refused (`ENODEV`).
- Small consecutive writes to a home file are gathered (64 KiB) until the
  process's next other call.
- `wine-home-1`/`-2` run only with an accelerator (prefix creation takes
  over 8 minutes under TCG).
- Version 0.53.0.

## Linux step 10c: ZooFS 2 (2026-11-01)

Design: docs/linux10c-design.md.

- Owner's choice: a real on-disk file system rather than patching Wine to
  link its DLLs. A Wine prefix is 880 files / 167 MB, and the S5 store
  (all data in memory, one snapshot in 8 MiB) could not hold it.
- Two alternating superblocks; data and metadata go only to blocks
  neither the current nor the previous generation uses, and the
  superblock is written last. Log wording kept from S5.
- Each save allocates first-fit from block 2.
- Changed blocks wait in a chunked slab; a save is forced at 8 MiB.
- No migration from `ZOOFS001` (only test disks had it).
- `BLOCK_READ_RUN`/`BLOCK_WRITE_RUN`: 16 blocks per request, the count in
  bits 48..64 of the block argument.
- Test data disks are 1 GiB.
- Version 0.52.0.

## Linux step 10b: `/tmp` in RAM; Wine runs `cmd.exe` (2026-11-01)

Design and findings: docs/linux10b-design.md.

- `/tmp` is a kernel tmpfs (replacing L10a's home `.tmp`). Its files are
  whole pages shared by name, descriptors and mappings; symbolic links
  live there only for now.
- `MAP_SHARED` maps a file's own pages. `MAP_PRIVATE` is copy-on-write,
  since Wine relies on read-only private views showing later changes.
- Per-thread GS base (`ARCH_SET_GS`) for Wine's Windows code.
- Inode numbers come from full paths for folders everywhere; Wine finds
  drives by them.
- Limits raised: 1,024 descriptors, 512 tasks, an 8 MiB kernel heap.
- Random numbers without RDRAND come from a non-cryptographic mixer; this
  is documented, not hidden.
- The `wine` image carries `/etc/ld-musl-x86_64.path` with Wine's Unix
  library folder.
- Input marks the kernel task busy, so a spinning program cannot starve
  the shell.
- The verifier gained `Step::Rest` for Wine's unordered process tree; the
  Wine case checks cmd's own output line and `wine`'s exit code.
  Version 0.51.0.

## Linux step 10a: Wine builds, starts and reaches its server (2026-10-31)

Design and findings: docs/linux10-design.md.

- Wine 11.0 is built from source (owner's choice) in WSL Alpine, lean and
  64-bit; exported as 958 files / 162 MB into a separate `wine` image.
- Linux strings (arguments, environment) are laid out as on Linux; Wine's
  preloader depends on it.
- `/tmp` is the home folder's `.tmp` for now; names starting with `.` are
  hidden in Mac-style listings. A RAM tmpfs is the next step (L10b).
- musl's `libc.so` no longer links Zig's compiler runtime, which hid
  `memcpy`/`memset`/`strlen`; three helpers come from c/musl/complex-mul.c.
- The `libc.musl-x86_64.so.1` alias proved unnecessary (a negative control
  passed): musl's loader treats any `libc.` name as itself. Removed with
  the tar hard-link support added for it. Version 0.50.0.

## Linux step 9: the address-space layout Wine needs (2026-10-31)

Design: docs/linux9-design.md.

- Each process has its own reservation window. Linux programs get the
  whole lower half (64 KiB to the stack), kernel-chosen mappings from
  0x7f0000000000 up, and 4096 reservations; ZooOS programs keep theirs.
- Program and loader images are reservations; the stack is an 8 MiB
  reservation committed as used.
- munmap and MAP_FIXED carve exactly their range out of whatever is there,
  as on Linux.
- Wine is to be built from source (owner's choice) in a WSL Alpine
  environment the owner enabled; Wine 11.0 and Alpine 3.24.3 downloads are
  verified (tools/wine/README.md, tools/alpine/README.md). Version 0.49.0.

## Linux step 8: dynamically linked programs (2026-10-31)

Design and measurements: docs/linux8-design.md.

- musl 1.2.5 source downloaded with the owner's approval, verified by
  SHA-256, Alpine's SHA-512 and musl's signature; `libc.so` (the loader)
  is built from it by xtask with the pinned Zig, linked with LLD directly.
- The kernel loads a program's `PT_INTERP` loader at 768 MiB (position-
  independent programs at 16 MiB) and passes `AT_BASE`.
- `mmap` maps initrd files as private copies and honours `MAP_FIXED`;
  `stat` gives distinct inodes; `membarrier` is a no-op on one CPU.
- A dynamic start costs about 0.4–1.1 Mcycles more than a static one
  (copying the loader). Version 0.48.0.

## Linux step 7: Unix sockets and event waits (2026-10-31)

Design and measurements: docs/linux7-design.md.

- A connected Unix stream socket is two L5 pipes; `SCM_RIGHTS`
  descriptors ride on the byte count they were sent with, so `recvmsg`
  returns them with exactly the right message.
- Bound names live in one kernel table ("@abstract", "/path"), freed on
  close; no socket files in the file system.
- One readiness function serves poll, select and epoll; waits block until
  the general wake-up or their timeout. epoll is level-triggered (EPOLLET
  treated as level); EPOLLONESHOT honoured.
- Non-blocking mode is a per-descriptor bit; would-block calls on pipes,
  sockets and eventfds return EAGAIN. Socket round trip under WHPX: about
  9,000 cycles. Version 0.47.0.

## Linux step 6: signal handlers, threads and lazy memory (2026-10-31)

Design and measurements: docs/linux6-design.md.

- Signal frames follow Linux's x86-64 layout exactly, so handlers can read
  and edit `ucontext_t` (Wine's fault handling). Delivery happens on the
  way back to user mode; blocked calls end with EINTR or restart under
  SA_RESTART; calls waiting on BadgerHandler or a vfork child are not
  interrupted.
- CPU faults in Linux threads become SIGSEGV/SIGILL/SIGFPE/SIGTRAP/SIGBUS
  for the program's handler, or end it with that signal.
- Linux threads reuse the native thread machinery; futexes reuse its wait
  fields; one BadgerHandler request at a time per process.
- Anonymous memory is committed on first touch; a system call that meets
  such a range commits it and runs again. pthread_create + join: 2.6M →
  20 thousand cycles. Version 0.46.0.

## Fast step 2: kernel output and memory reuse (2026-10-31)

Design and measurements: docs/f2-design.md.

- Serial output is buffered and sent 16 bytes per `rep outsb` without
  waiting; fatal paths flush. A PC without a COM port gets no serial I/O.
- The text console scrolls its text grid and redraws once per tick or
  idle moment instead of copying the framebuffer per line.
- The frame allocator reuses the most recently freed frames first, and
  ended programs are reaped at every switch.
- fork + wait under WHPX: 9.4–12.5 → 1.5–2.5 Mcycles. Version 0.45.0.

## Linux step 5: processes and pipes (2026-10-31)

Design and measurements: docs/linux5-design.md.

- `fork` copies the address space; copy-on-write is deferred until a
  measurement shows it matters. Copying 4 MiB adds about 1–3 Mcycles, per
  the F2 measurement. L5's first claim that 4 MiB cost nothing was wrong:
  the compiler had removed the buffer.
- `vfork` / `clone(CLONE_VM|CLONE_VFORK)` (musl's `posix_spawn`) copy
  nothing: the child borrows the parent's space until `execve` or exit.
- Each child gets its own BadgerHandler session (an `Open` on the
  parent's channel), so requests from two programs never share a channel.
- Pipes are 64 KiB rings of physical pages, not kernel-heap memory.
- Signals have their default actions only; handlers are L6.
- The fork timing exposed that one kernel log line costs about 1 ms
  (polled serial plus a full-framebuffer console scroll). That is F2.
  Version 0.44.0.

## Linux step 4: writable files in the home folder (2026-10-31)

Design and measurements: docs/linux4-design.md.

- Linux programs started from the shell get the home folder; paths under
  `/home/user` go to BadgerHandler, the rest stays the read-only initrd.
- The kernel acts as BadgerHandler's client inside a system call by
  sending one request, blocking, and re-running the call on the reply,
  which replays earlier replies of the same call (the existing
  block-and-re-run mechanism; nothing else waits).
- Each such program gets a kernel-owned 64 KiB shared buffer (F1): 1 MiB
  reads take 0.7–0.9 Mcycles instead of 45–57. Version 0.43.0.

## Fast files step 1: shared-memory bulk transfers (2026-10-31)

Design and measurements: docs/f1-design.md. Owner direction: speed first;
measure, then change.

- Shared-memory objects (calls 59-61, ABI version 17): reference-counted
  pages, freed at the next reap after the last handle or mapping goes.
- BadgerHandler sessions can attach a 64 KiB shared buffer; `ReadBulk`
  and `WriteBulk` move up to 64 KiB per round trip. `zoo_rt`'s
  `file_read`/`file_write` use it above 2 KiB, so every native program
  gains without changes. The file service stays an isolated process; a
  kernel file cache is a later option, if measurements call for it.
- The timer no longer gives an idle kernel task the CPU while a program is
  ready (it halted until the next tick: up to 10 ms lost). Version 0.42.0.

## Linux step 3: folders, working directory, uname, environment (2026-10-31)

Design: docs/linux3-design.md.

- Initrd folders are path prefixes; folder descriptors, `getdents64`, a
  per-process working directory with `.`/`..`, a fixed environment.
- `uname` reports `Linux` (release `6.1.0-zooos`, version `ZooOS …`), as
  other kernels' Linux layers do. Version 0.41.0.

## Linux step 2: memory, read-only files and time (2026-10-31)

Design: docs/linux2-design.md.

- Linux `mmap`/`munmap`/`brk`/`mprotect` are built on ZooOS's
  reserve/commit calls; files are read-only from the initrd with a
  per-process descriptor table; `clock_gettime` and relative sleeps use
  the existing clock and sleep. Blocked Linux calls are re-run by the
  Linux handler. Version 0.40.0.

## Linux step 1: static musl programs run (2026-10-31)

Design: docs/linux1-design.md.

- The `syscall` instruction is the Linux entry (native programs keep
  `int 0x80`); its frame joins the existing trap path. Programs packed
  under `linux/` start with the System V stack. Nine Linux calls are
  implemented; others return `ENOSYS` and are logged once. The ZooOS ABI
  version is unchanged (16). Version 0.39.0.

## Owner decision: C library through Linux system calls (2026-10-31)

Of the routes in docs/feasibility/libc-route.md the owner chose route 3:
the kernel gains a Linux x86-64 system-call personality, so stock musl
and static Linux programs run unmodified, grown one tested step at a
time. ZooOS's own calls (`int 0x80`) stay for native programs.

## Desktop step 9: double-click to zoom (2026-10-31)

Design: docs/d9-design.md.

- A double-click on a title bar zooms a window to fill the desktop area,
  and restores it the second time. Version 0.37.0.

## Desktop step 8: resize windows (2026-10-31)

Design: docs/d8-design.md.

- Windows resize from their bottom-right corner (minimum 200×120, kept
  on screen). Version 0.36.0.

## Hardware step 5: key repeat for USB keyboards (2026-10-31)

Design: docs/h5-design.md.

- The kernel repeats the last pressed USB key (500 ms delay, 30 ms rate)
  as set-1 make codes, as a PS/2 keyboard would. Harness gained
  `HoldKey` and prefix markers. Version 0.35.0.

## Hardware step 4: USB hubs (2026-10-31)

Design: docs/h4-design.md.

- The xHCI driver sets up hubs on root ports (one tier) and the devices
  behind them, with route strings and transaction-translator fields.
  The test option `usb` became none / direct / hub. Version 0.34.0.

## Hardware step 3: the local APIC timer (2026-10-31)

Design: docs/h3-design.md.

- The system tick is the local APIC timer, calibrated against the ACPI PM
  timer found through the FADT; it uses the PIT's vector, so the trap
  path is unchanged. Without ACPI, a PM timer or an APIC the PIT stays.
- Test harness action `WaitGap` checks guest time against host time (500
  ticks must take 4–6 s). Version 0.33.0.

## Hardware step 2a: ready for a USB stick (2026-10-31)

Plan and risks: docs/usb-boot.md.

- `cargo xtask usb-image` builds the everyday image into
  `build/usb/zoo-os-usb.img` and prints its SHA-256. It never writes a
  device: the owner writes the stick, on a device they name, and boots it
  only on another PC.
- The desktop uses the top-left 1920×1200 of a larger screen instead of
  refusing it; test-only kernel mode `big-screen` exercises this in QEMU.
  Version 0.32.0.

## Hardware step 1b: USB keyboard and mouse input (2026-10-31)

Design: docs/h1b-design.md.

- HID boot protocol, polled on every timer tick. USB keyboard reports are
  translated to scancode set 1 and USB mouse reports to mouse events in
  the existing queues, so nothing above the kernel changes.
  Version 0.31.0.

## Hardware step 1a: USB host controller, enumeration (2026-10-31)

Design: docs/h1-design.md.

- A polled xHCI driver in the kernel finds the controller (bus 0 and
  buses behind bridges), takes it from the firmware, and enumerates root
  ports (addresses devices, reads descriptors, logs HID keyboards and
  mice). Towards real hardware booted from USB; input is H1b.
- Device registers are mapped uncached in a new kernel MMIO region.
- Test cases attach USB devices only when asked (`usb` flag), so they do
  not take input from the PS/2 devices the other cases use.
  Version 0.30.0.

## Desktop step 7: Files browses the home folder (2026-10-31)

Design: docs/d7-design.md.

- Files shows the person's home folder (from BadgerHandler), not the
  system's initial filesystem: folders open, text files preview,
  Backspace goes up. The desktop checks moved to the home listing.
  Version 0.29.0.

## Desktop step 6: start in the desktop (2026-10-31)

Design: docs/d6-design.md.

- The fast-boot build (what `cargo xtask run` boots) starts the desktop
  session by itself; Escape leaves it for the debug shell. Test builds
  stay in the shell. Version 0.28.0.

## Desktop step 5: date and time in the menu bar (2026-10-31)

Design: docs/d5-design.md.

- The menu bar shows `Sat 31 Oct 06:31 UTC` instead of the uptime (which
  remains the fallback without a clock). UTC is labelled: no time zones
  until zone rules exist, since a fixed offset breaks at daylight-saving
  changes. Version 0.27.0.

## Boot step 1: fast boot without the check programs (2026-10-31)

Design and measurements: docs/b1-design.md.

- The boot-check programs (20 test processes, about 1 s) are tests, not
  part of starting the system. Kernel feature `fast-boot` skips them; the
  kernel's own self-tests (about 0.1 s) stay. `cargo xtask run` boots the
  fast-boot build; `verify` keeps running every check on the normal build
  and adds a `fast-boot` case.
- The harness records when each serial line arrives
  (`build/runs/<case>/serial-times.log`), for boot timing.
- Mode features are checked with a count (at most one) instead of a list
  of pairs. Version 0.26.0.

## Desktop step 4: Notes (2026-10-31)

Design: docs/d4-design.md.

- A plain-named app, `notes` (Notes), edits `Documents/notes.txt` in the
  home folder: append-only editing, autosave one second after typing
  stops and on close. Opened from the dock; not started with the session.
- Text files may use CRLF; apps that show text treat CRLF as a line
  break. Version 0.25.0.

## POSIX step 5b-1: process arguments and the clock (2026-10-31)

Design: docs/posix5b1-design.md. Step 5b (libc port) is split: 5b-1
arguments and time, 5b-2 file descriptors, then the libc port itself.

- Command lines ride in `spawn`'s name argument (program, then NUL-
  separated arguments), so existing callers are unchanged; call 57
  `args_get` returns them.
- Wall-clock time from the CMOS RTC read once at boot (UTC) plus timer
  ticks; call 58 `clock`. 10 ms resolution. ABI version 16; version 0.24.0.

## Desktop step 3: a dock and close buttons (2026-10-31)

Design: docs/d3-design.md.

- Close buttons at the right end of title bars (macOS has them at the
  left; the right keeps existing title positions). A closed app ends
  normally (bounded wait, then kill) and is not restarted.
- An auto-hiding dock above the status line reopens closed apps and
  raises running ones. Version 0.23.0.

## Desktop step 2: redraw only what changed (2026-10-31)

Design: docs/d2-design.md.

- PeacockHandler tracks damaged rectangles (up to 8; beyond that, or over
  half the screen, a full repaint), draws clipped to each and copies only
  those to the framebuffer.
- Test keys F9/F10/F11 for measuring and for comparing against a full
  repaint. Measured under WHPX: a pointer-move frame went from about 4.2
  million to about 31,500 cycles (docs/d2-design.md §5). Version 0.22.0.

## Desktop step 1: a mouse (2026-10-31)

Design: docs/d1-design.md.

- PS/2 mouse in the kernel (IRQ12); events reach the display owner
  through a new call, 56 `mouse_read`, rather than the keyboard stream
  (§4 says why). ABI version 15.
- PeacockHandler keeps window positions and stacking order: a click
  focuses a window and raises it, a title-bar drag moves it. Tab still
  cycles focus. Version 0.21.0.
- QEMU's mouse is relative; an absolute pointer (USB tablet) and real
  hardware mice (USB HID) are later work.

## Services step 5: persistent files (2026-10-31)

Design: docs/s5-design.md.

- A virtio block driver in the kernel (legacy interface, polled) for a
  second QEMU drive; only the resident supervisor can attach it, and it
  passes the handle to BadgerHandler (`disk` flag).
- BadgerHandler keeps `/home/user` in two A/B snapshot slots with
  generation and CRC-32; it loads the newest valid one, saves 50 ticks
  after the last change, payload before header. Recovery behaviour is
  documented in docs/s5-design.md §3, as DECISIONS.md required before
  trusting personal data to disk.
- Shell `dir`/`cat`/`put` for the home folder. Calls 52–55; ABI version 14.
  Version 0.20.0.

## Services step 4b: the filesystem as a service (2026-10-30)

Design: docs/s4-design.md.

- The file tree moved from the kernel into BadgerHandler (a user-space
  service); a folder handle is a channel to it. Programs kept the same
  `zoo_rt` functions. Supersedes the in-kernel filesystem decisions of
  M5c-1 and POSIX step 4 (their behaviour is unchanged).
- Calls 19–25 and 41–45 return `NOSYS` (numbers reserved); ABI version 13.
  Handle and `wait_any` limits raised to 64.
- Boot checks use a private BadgerHandler; the resident supervisor runs one
  as a service and hands out home-folder handles (the desktop gets one via
  the kernel at session start).
- Shell command `restart <service>`. Version 0.19.0.

## Services step 4a: wait_any and faster hand-offs (2026-10-30)

Design: docs/s4a-design.md.

- Call 51 `wait_any` (up to 16 handles, timeout); ABI version 12. Services
  block in it instead of sleep-polling.
- Scheduler: voluntary switches prefer a ready user task over the idling
  kernel task; timer ticks stay plain round-robin.
- Measured (TCG, 50 ping round trips): 250 ticks before, 50 with
  `wait_any`, 0 with the scheduler change too.
- Harness `WaitFor` accepts the wanted line after shell prompts.
- Version 0.18.0.

## Services step 3: handles in messages (2026-10-30)

Design: docs/s3-design.md.

- Channel messages are bytes or one moved handle with a 64-bit tag; the
  wrong receive call returns `WRONGKIND` and leaves the message queued.
- Handles still queued towards a closing end are released (the queue is
  taken out under the lock and released after, so a channel's own end in
  its queue cannot deadlock). An end cannot be sent through itself.
- Services find each other by name: `connect <name>` to Zookeeper, which
  creates a channel and moves one end to each side (tags 1 and 2).
- Calls 49 `send_handle`, 50 `recv_handle`, error 19 `WRONGKIND`; ABI
  version 11. Version 0.17.0.

## Services step 2: log routing (2026-10-30)

Design: docs/s2-design.md.

- Program log lines still go to the serial console and are also copied
  into a 128-line kernel buffer; BeaverHandler drains it through a
  log-reader handle that only the resident supervisor can obtain and pass
  on (`etc/services` flag `logs`).
- `logs [text]` filters; at most 30 lines per answer.
- Calls 47 `log_attach`, 48 `log_read`; ABI version 10. Version 0.16.0.
- Name lookup moves to S3 because it needs handles passed in messages.

## Services step 1: resident supervisor (2026-10-30)

Design: docs/s1-design.md.

- After the boot checks and the leak check, the kernel starts a second
  Zookeeper in supervise mode that stays running; the check run stays
  separate so the leak check remains exact.
- Services come from `etc/services` (`seed/etc/services`), are pinged every
  second (2 s to answer), restarted with back-off `10 × 2^n` ticks up to a
  per-service limit, then left `failed`.
- First services: BeaverHandler (logs; beavers handle logs) and seal-test
  (an Arctic test service that can be crashed or hung on request).
- Shell commands `services`, `logs`, `poke`; call 46 `process_id`; ABI
  version 9. Version 0.15.0.

## Program names (2026-10-30)

Owner decision. `…Handler` is reserved for system services (daemons):
PeacockHandler (window server), OtterHandler (file broker), ElephantHandler
(model service). Apps have plain names: Files, Assistant, Crash Test
(`files`, `assistant`, `crash-test`). Boot-time tests use Arctic animals
with a `-test` suffix: beluga (messaging), lemming (crash/restart), muskox
(preemption), narwhal (floating point), arcticfox (files), caribou
(threads), walrus (address space), wolverine (exceptions), puffin (C).
Zookeeper remains the supervisor. Earlier entries and verification sections
keep the names used at the time.

## POSIX layer step 5a: C toolchain (2026-10-30)

Design: docs/posix5-design.md. The owner approved downloading a C
toolchain into the project folder on 2026-10-30.

- Zig 0.16.0 (`zig cc`: clang 21.1.0, lld) in `tools/zig/`, pinned by
  version and SHA-256 (tools/zig/README.md); 0.16.0 rather than the
  four-week-old 0.17.0. One archive gives compiler, linker and musl sources.
- C programs are freestanding for now (no libc), linked like the Rust user
  programs; `zoo_abi.h` is generated from the `zoo-abi` crate.
- The toolchain is optional for building: without it C programs are left
  out and their verify case reports `SKIP`.
- Step 5 is split: 5a toolchain + freestanding C (done), 5b libc port.
- Version 0.14.0; ABI unchanged (8).

## POSIX layer step 4: writable files (2026-10-30)

Design: docs/posix4-design.md.

- Path-based file calls through directory handles (create, write, truncate,
  remove, rename) and a new WRITE right; open-file handles and descriptors
  come later.
- File bytes live in whole physical pages (the kernel heap holds only page
  lists); seeded files are copied on first write. Still not persistent.
- Limits: 16 MiB per file, 64 MiB of file pages in total.
- The boot leak check counts file pages as free and audits the counter
  against the pages files hold (a check that would otherwise hide leaks).
- Calls 41–45, ABI version 8. Version 0.13.0.

## POSIX layer step 3: exceptions (2026-10-30)

Design: docs/posix3-design.md.

- One handler per process, an optional exception stack per thread; the
  kernel writes a full register context (including FXSAVE) and the handler
  resumes with `exception_return`, possibly after editing it, or declares
  the fault unhandled.
- Faults inside a handler are always fatal; returned contexts are checked
  (user-half `rip`/`rsp`/FS base, valid MXCSR, only arithmetic and
  direction flags taken from `rflags`).
- Guard pages are left to user space (reserved pages committed by the
  handler) instead of a kernel no-access page state.
- Calls 38–40, ABI version 7. Version 0.12.0.

## POSIX layer step 2: address space (2026-10-30)

Design: docs/posix2-design.md.

- Windows-shaped calls (reserve, commit, protect, decommit, release, query)
  rather than `mmap` first: they map one-to-one onto what Wine needs, and
  `mmap` can be layered on them later.
- A fixed window `[0x4000_0000, 0x1000_0000_0000)`; 64 KiB-aligned
  kernel-chosen reservations; ≤ 256 reservations and ≤ 1 GiB committed per
  process, checked before allocating.
- Page state is kept only in the page tables (the kernel heap is too small
  for per-page records); a subtree-skipping walker makes large ranges cheap.
- Guard (no-access) pages wait for exception delivery (step 3).
- Calls 32–37, ABI version 6. Version 0.11.0.

## POSIX layer step 1: threads (2026-10-30)

Design: docs/posix1-design.md. First step of the owner-chosen strategy A
(docs/feasibility/wine-hosting.md).

- Processes and threads are split: a `Proc` holds the address space,
  handles and memory cursors; each thread is a scheduler `Task`.
- Calls 27–31 (`thread_create`, `thread_exit`, `futex_wait`, `futex_wake`,
  `set_fs_base`), error 18 `TIMEDOUT`. ABI version 5. Version 0.10.0.
- Any thread's `exit`, crash or kill ends the whole process; the last
  `thread_exit` ends it too.
- Futex waits may return spuriously (the kernel's general wake-ups restart
  blocked calls); callers re-check. `futex_wake` also counts waiters that a
  general wake-up already made runnable.
- Kernel stack slots 16 → 32.
- Threads come before a libc: they need no host tools, and Wine needs them.

## Resolved in M5c-2 (2026-10-30)

Design: docs/m5c2-design.md.

- Guided decoding instead of free-form tool calls: the model picks one of
  four tool words by next-token score and writes the argument with tokens
  restricted to path characters. A 55-token system prompt is computed once
  per model service and reused by rewinding the cache. Chosen because a
  conventional tool prompt (≈ 300 tokens) is minutes per answer even with
  WHPX and close to an hour under TCG.
- Typed commands bypass the model; `create` always shows the broker's plan
  and waits for `y`.
- New programs: ParrotHandler (the Assistant window); ElephantHandler gains a
  service mode. The model service never holds a directory handle.
- The window server now runs three windows: Files on the left, Chimp and the
  Assistant stacked on the right (existing screen checks keep their
  positions). The session Zookeeper passes a home handle down; `fs_root`
  is closed right after narrowing.
- Verify gained `assistant` (TCG, no model), `assistant-model` (TCG, model
  image: responsiveness and cancellation) and `assistant-live` (model
  choices end to end, only with `--accel`, otherwise `SKIP`). The model's
  choices are also checked on the host by `cargo xtask assist-check`.
- OtterHandler's `search` matches all query words against path or contents.
- Version 0.9.0. ABI unchanged (version 4).

## Resolved in M5c-1 (2026-10-30)

Design: docs/m5c1-design.md.

- In-memory filesystem in the kernel: `/bin` (read-only) and `/home/user`
  (seeded from `seed/home/user/`, writable). Not persistent; a disk driver
  and crash consistency stay deferred.
- Directory capabilities with rights READ/CREATE/REMOVE. Paths are always
  relative to a handle; empty, `.` and `..` components and absolute paths are
  rejected, so no path can leave its handle's subtree. No symbolic links.
- `fs_root` only for processes the kernel started (boot Zookeeper, session
  Zookeeper, programs the shell `run`s), once each.
- Calls 19–26; directory calls take an `FsArgs` struct pointer. ABI
  version 4. Version 0.8.0.
- The action broker is a separate process (OtterHandler) holding the home
  handle the assistant will act through; its `undo` removes only folders its
  last `create` made, and only while empty. No delete or file-write tool.
- `seed/` files are stored byte-exact (`.gitattributes`: `-text`) so their
  sizes do not depend on the checkout's line-ending settings.

## Resolved in M5b (2026-10-30)

Design: docs/m5b-design.md.

- Inference engine `zoo-infer`: `no_std` + `alloc`, no unsafe code, one
  dependency (`libm`). The same code runs in the guest and on the host.
- Supported formats: F32, Q4_0, Q8_0 (all the chosen GGUF uses). Weights are
  read in place from the `fs_map`ped file; only norms and biases are
  expanded.
- Correctness evidence: unit tests, plus an independent NumPy reference
  (`tools/reference/`, project-local venv, numpy 2.3.4) compared token for
  token by `cargo xtask infer-check`, plus the in-guest `inference` case
  that must reproduce the host engine's tokens exactly.
- The model travels in a separate model image; other cases keep the small
  image. Missing model: verify stops unless `--skip-model` (case reported
  as not run).
- Verify options `--accel tcg|whpx|kvm` (default tcg; accelerators optional)
  and `--case NAME` (iteration only). Under accelerators QEMU's default CPU
  model is used (`-cpu max` made OVMF fault under WHPX).

## Resolved in M5a (2026-10-30)

Design: docs/m5a-design.md.

- User programs are built for `user/x86_64-zoo.json` (SSE enabled, normal
  float ABI, static, small code model) with `-Zbuild-std`. The kernel stays
  soft-float on `x86_64-unknown-none`.
- Eager per-task FXSAVE/FXRSTOR when switching between user tasks; x87 and
  SIMD faults end only the faulting process.
- `mem_alloc` (syscall 17): grow-only heap at `0x0000_1000_0000_0000`,
  1 GiB per process. `fs_map` (18): read-only, zero-copy initrd file
  mappings at `0x0000_2000_0000_0000`. ABI version 3.
- Guest RAM 2 GiB (was 512 MiB).
- Version 0.7.0.

## Feasibility outcomes (2026-10-30)

Desk studies: docs/feasibility/wine-hosting.md, docs/feasibility/gpu.md.

- Wine hosting: BLOCKED. Wine's Unix side needs a POSIX-like host (libc,
  threads, memory mapping, exceptions to user handlers, sockets, a writable
  filesystem, a display driver). Recommended path if kept: grow a POSIX layer
  and port Wine (as Haiku did), not a native re-implementation.
- GPU and graphics translation: BLOCKED. WoW needs DXVK/vkd3d-proton over a
  Vulkan driver over a kernel GPU driver; none exist, and stock QEMU on a
  Windows host offers no 3D test bed. Recommended reference GPU if pursued:
  current AMD Radeon (open driver stack).
- **Open for the owner:** whether to keep Opera/WoW as goals with that
  programme, which test bed and reference GPU, or to narrow the goals.

## Resolved in M4 (2026-10-30)

Design: docs/m4-design.md. Evidence: docs/verification.md.

- The desktop is user space: PeacockHandler (window server) owns the
  framebuffer and keyboard through two new system calls; applications are
  separate processes talking a small text protocol over channels.
- ABI version 2: calls 11–16 (`ticks`, `sleep`, `try_recv`,
  `display_acquire`, `input_read`, `fs_stat`), errors 13 `BUSY` and 14
  `AGAIN`. Existing numbers unchanged.
- One display owner at a time; the framebuffer is mapped at
  `0x0000_4000_0000_0000` in the owner and unmapped (never freed) when it
  ends. While owned, the kernel console stops drawing and the shell leaves
  the keyboard alone.
- The window server polls input and app channels and sleeps one tick when
  idle, rather than a multi-object wait (deferred).
- Back-buffered drawing (whole frames copied to the framebuffer); text is
  blended with an exact integer formula the verifier reproduces.
- Supervision in two layers: the window server restarts crashed apps (3
  times each); Zookeeper restarts a crashed window server (3 times per
  session). Escape ends the session; F12 is a documented test key that makes
  the window server fault.
- The shell command `desktop` starts the session through a kernel-owned
  control channel; boot still runs the M3 scenario and ends in the shell.
- Verifier: multi-step QMP interactions, several screenshots per case,
  unordered marker groups for concurrent events.
- Version 0.6.0.

## Resolved in M3 (2026-10-30)

Design: docs/m3-design.md. Evidence: docs/verification.md.

- Address space split three ways: user (level-4 entries 0–255, per
  process), bootloader mappings (256–383, via `dynamic_range_start/end`),
  kernel-owned (384–511). Replaces M2's "bootloader in the lower half".
- System calls via `int 0x80` (ABI version 1, numbers 0–10, negative errors
  1–12). `syscall`/`sysret` deferred.
- One assembly entry path (`kernel/src/trap.rs`, naked functions) for the
  timer, system calls and user-reachable faults; a context switch is
  returning another task's saved frame.
- Preemptive round-robin at 100 Hz. The kernel task is preempted only at its
  idle point; blocked system calls are re-run when the task is next
  scheduled after any wake-up.
- `Memory` moves to a global used only with interrupts disabled (replaces
  M2's single owner passed by `&mut`).
- Handles: 16 slots per process, values carry a generation so stale handles
  are rejected. Channels: bidirectional, 8 × 256-byte messages per direction,
  non-blocking send.
- Programs: static ELF at `0x200000` (`-C relocation-model=static`), loaded
  from a ustar initrd passed as the bootloader ramdisk; parsers in the new
  dependency-free `zoo-abi` crate, unit-tested on the host.
- Zookeeper (supervisor) runs a fixed start-up plan and exits; restart
  policy is one restart. LionHandler, MonkeyHandler and HamsterHandler are
  the first handlers. A long-lived Zookeeper is deferred.
- User log text is prefixed `[pid:name]` and sanitised (`ZOO_OS` →
  `ZOO-OS`) so user programs cannot forge verifier markers.
- Version 0.5.0.

## Rename to ZooOS (2026-10-30)

- Project name ZooOS, identifier `zoo-os` (crates `zoo-os-kernel`, images
  `zoo-os-<mode>-uefi.img`), serial markers `ZOO_OS …`, shell prompt `zoo> `.
  Replaces the placeholder OriginsOS everywhere except past verification
  results, which are kept as recorded, and the local checkout folder name.
- Component naming theme: the M3 runtime/service supervisor will be
  **Zookeeper**; services and workers are animal-named handlers
  (`LionHandler`, `ZebraHandler`, …). Names are given only to components that
  exist; none exist yet.
- Name check (web, GitHub, crates.io): no OS or product called ZooOS;
  `zooos` and `zoo-os` free on crates.io. Apache ZooKeeper (a distributed
  coordination service) shares the runtime's name; accepted by the user,
  since searches target "ZooOS" and Zookeeper is an internal component.
  No legal trademark search has been done.
- Version 0.4.0, because the serial marker contract changed.

## M0 must resolve and record

(Resolved above; kept for reference.)

- Exact Rust release or dated nightly supported by the selected bootloader build
  path. Do not guess a version; test it and add rust-toolchain.toml.
- Exact required components and targets, and whether bootloader construction
  needs nightly or build-std. Limit unstable features to demonstrated needs.
- Host prerequisites, linker requirements and tested host OS. Windows PowerShell
  is the user's current environment; a Linux development host would not change
  the guest foundation, but do not silently make WSL mandatory.
- QEMU version, OVMF provenance, licensing, filenames and checksums. Resolve
  configuration through documented paths, not a developer's absolute paths.
- Exact image-generation procedure supported by the bootloader version.

## Deliberately deferred

Kernel structure beyond M1; scheduler policy; user executable format; persistent
filesystem; syscall ABI; IPC serialization; GPU and driver model; UI toolkit;
Windows compatibility host interface; model runtime and hardware acceleration.
Do not invent fixed addresses or public ABIs in this document before validating
the boot contract. Future memory maps must distinguish physical/virtual addresses,
reserved regions, guard pages, ownership and access permissions.

Before M3, document syscall numbering/versioning, handle lifetime, IPC errors,
process exit semantics and executable loading. Before exposing writable storage,
document crash recovery and filesystem consistency. Before loading device drivers,
document interrupt, DMA and ownership boundaries.

## Source references

- https://docs.rs/bootloader/0.11.17/bootloader/
- https://docs.rs/bootloader_api/0.11.17/bootloader_api/
- https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html

These references justify the starting direction, not a tested local toolchain.

## Voice models (2026-11-04)

The owner approved docs/voice-plan.md as recommended.

- **Speech to text: Moonshine Tiny** (27 M parameters, MIT), for speed on
  short commands. Downloaded as `model.safetensors` from
  `moonshine-ai/moonshine-tiny` (the repository moved from
  `UsefulSensors/`), not the ONNX files the plan listed: the same weights
  in a format readable without an ONNX runtime, which the host does not
  have.
- **Speech: Kokoro-82M** (Apache-2.0) with the `af_bella` voice; the CC BY
  training-data attribution is accepted.
- **No GPL espeak-ng, no openWakeWord pre-trained models.** The wake word
  is "hey Bella" at the start of a transcript.
- **Silero VAD** (MIT) is approved, fetched when voice detection is built.
- Pinned in tools/models/README.md ("Voice") and `tools/models/voice/*.sha256`.

## Hearing (2026-11-04)

Version 0.110.0. Design: docs/hearing-design.md.

- **Converted to GGUF, F32:** the engine's reader, model files and
  parallel products serve Moonshine as they serve the chat model; 8-bit
  weights were not needed (0.1 s for a command on 8 processors).
- **Test speech from the host's own synthesizer** (Windows System.Speech),
  so the sentences are known and nothing is downloaded. The recordings are
  made on demand under `build/voice/`; on a host without it the hearing
  cases are skipped, as the Wine cases are without their export.
- **The model and recordings in a dedicated boot image** for now; the
  model disk keeps its two models until hearing has a microphone to serve.

## Microphone (2026-11-04)

Version 0.111.0. Design: docs/microphone-design.md.

- **16 kHz mono from the device,** the format speech to text reads, so the
  guest never resamples.
- **Reads never wait; a two-second ring** drops the oldest samples when
  nobody reads, as playback never waits for the speaker.
- **One owner,** the first reader, until it exits; recording stops with
  it. The hearing service is meant to claim it at start.
- **SDL's disk driver as the test microphone,** with QEMU's mixing engine
  off: the only QEMU input on Windows that reads a file, and with the
  engine on the samples were changed. The PC's own microphone is never
  used.

## Listening (2026-11-04)

Version 0.112.0. Design: docs/listening-design.md.

- **An energy voice detector instead of Silero VAD, for now.** Silero is
  published as ONNX or TorchScript only; running it means an ONNX runtime
  of our own. The energy detector needs no download and is tested exactly;
  Silero stays approved for noisy rooms.
- **BatHandler, a helper Bella starts,** as she starts her model, broker
  and file index: the microphone is hers while she runs.
- **Only when a hearing model is installed,** so desktops without one keep
  their processes (and the process numbers the cases expect).
- **The gate is word-based** (`addressed`), and typed requests use it too.
- **The hearing model is not on the everyday model disk yet,** nor a
  microphone in `cargo xtask run`; a separate step, as it changes the
  processes of every desktop with models.
- **The microphone ring grew to 8 s:** a reader busy hearing an utterance,
  or a machine starting its desktop, reads late.

## Voice detection after the first real voice (2026-11-04)

Version 0.114.0. docs/listening-design.md §6.

- **Background = the quietest frame of the last 3 seconds** (minimum
  statistics), digital silence left out, instead of a level that rose
  2.5 dB a second: it took a microphone's opening silence as the room.
- **One letter off "Bella" counts after a greeting.** Speech to text
  misspells names; without a greeting the name stays exact, so ordinary
  sentences ("Della said ...") are not taken as requests.
- **The hearing test is SnowyOwlTest:** `ermine-test` was already a Linux
  test program's name.
- **Her name, a pause, then the request** is one request (5 s); people
  pause after a wake word.
- **Speech not for Bella is never logged as words.** The microphone hears
  whatever is said in the room.
- **Recording drains on the timer tick,** not only on reads.
- **xtask's children end with xtask** (a Windows job object): a stopped run
  must not keep the microphone open.

## The action registry (2026-11-04)

Version 0.115.0. Design: docs/registry-design.md. The owner's direction:
Bella must be data-driven, so a new app feature reaches her without
changing her.

- **One file per app in the boot image** (`etc/actions/<program>.actions`),
  not announcements from running apps: actions of a closed app must work.
- **Plain text, line per key,** read by ElephantHandler (the model's
  commands and prompt), Bella (slots, asking, dispatch) and PeacockHandler
  (checks). `zoo_abi::actions` is gone.
- **Slots are filled from the request's own words** by the entry's verbs,
  places and joins; the model writes only a single text slot no words
  fill. An action the request cannot fill is no command.
- **Command words must start with distinct first tokens;** a colliding
  entry is left out and logged.
- **`delete` actions are always asked about,** by the asking app; the
  window server passes them on.
- **The newest matching line wins** for remove and change, until notes can
  be named as targets.
- **The system's own commands stay in code** for now: files and windows
  are served by the broker and the window server, not by an app.

## One Bella (2026-11-04)

Version 0.116.0. Design: docs/one-bella-design.md. From the owner's demo
(three Bellas, out of memory) and the outside review ("enforce the
singleton in service management, not only the window server").

- **A kernel name claim (`CLAIM`, ABI 21),** held by one live process and
  released at exit: a primitive like the display's, the network's and
  the microphone's single owners, usable by any service.
- **Two guards:** the window server never opens a second Bella window,
  and a second Bella process leaves before loading anything.
- **Learning only from corrections** until "ask, then learn" exists; what
  was learned before is set aside, kept but unread.
- **Words, not guesses,** for layouts the window server cannot make and
  splits with too few apps named (`say`, never scored by the model).

## Layouts (2026-11-04)

Version 0.117.0. Design: docs/layouts-design.md. The owner moved this up:
"say i say split these in rows, shouldn't it stack them then say is that
what you meant and then i confirm and it commits that to memory?"

- **One general ability, a split tree,** rather than a feature per
  arrangement: learning maps words onto abilities; it cannot create them.
- **Rules first for layout words,** before the model: exact, quick, and
  usable without the model.
- **Learned in general form** (`*`, `...`), so a phrasing works with the
  windows open now.
- **A spoken or typed yes is the confirmation** that lets a layout be
  learned; a no followed by a corrected layout teaches both requests.

## Targets and results (2026-11-04)

Version 0.118.0. Design: docs/targets-design.md. From the outside review.

- **Several matches ask which,** with the real lines; no guessing for
  removal or change. "Newest wins" is gone.
- **Results carry the request id and a status** (`ok`, `err`, `choose`) in
  the existing text messages, extended with the unit separator, not a new
  message format: the window server passes them through.
- **Questions expire after 60 s;** "don't ..." does nothing.

## Regression sentences and registry phrasing (2026-11-04)

Version 0.119.0. Design: docs/regression-design.md.

- **Bella's routing moved into `zoo_infer::route`, unchanged,** so the
  host check runs her code and not a copy. zoo-infer now depends on
  zoo-abi (both in this repository).
- **Baselines are per model and any change fails** until accepted; the
  baseline file's diff shows what changed.
- **Only sentences said to Bella come from the demos;** overheard speech
  is never copied into the repository.
- **A request in an action's registered words runs without the model**
  (verb at the start, a place of two words or more at the end), as the
  last check before the model.

## Staged wake and the pill (2026-11-04)

Version 0.120.0. Design: docs/wake-design.md.

- **The pill is drawn by the window server and decided by Bella:** she
  knows when she listens and works; the server only draws, and takes it
  down after 10 s without word, so it cannot outlive her.
- **A dark capsule at the top centre with five bars,** not a microphone
  icon: the owner asked for something Apple-like.
- **Her bare name followed by "said", "is", "and" and the like is talk
  about her.** A greeting or a mark keeps it a request.
- **A step of a multi-step request is never a correction,** so nothing is
  learned from it.

## Asking when unsure (2026-11-04)

Version 0.121.0. Design: docs/unsure-design.md. Bella asks before a
model choice that closes, moves or changes something when its margin is
under 5.00, a threshold measured on `assist-check`. Undoable choices never
ask, so most requests stay one step.

## Routines (2026-11-05)

Version 0.122.0. Design: docs/routines-design.md. A routine is a
confirmed phrasing in Bella's existing memory, with the command `steps a
| b`. There is no new store and no new file: forgetting, limits and
matching stay the same. Saving is always asked.

## Templates and forgetting (2026-11-05)

Version 0.123.0. Design: docs/templates-design.md. Templates generalise
over one thing only, the app named, with every other word the same, so a
pattern can't run something the user didn't mean. Forgetting a phrasing is
asked first, like any delete.

## Follow-ups (2026-11-05)

Version 0.124.0. Design: docs/follow-design.md. For 8 s after a spoken
request she takes a follow-up without her name, but only a command she
does without the model. Its words are logged only once it is taken, and
memory isn't asked about it, because the lookup is audited with the words.

## Model refinement (2026-11-06)

Version 0.125.0. Design: docs/refine-design.md. Two changes, measured:

- one "none" example in the prompt (`Set an alarm for seven`), worded
  unlike any test sentence;
- left and right read from the words.

Two prompt variants that lost right answers were dropped.

## Conversation, step 1: half-duplex and barge-in (2026-11-06)

Version 0.131.0. Design: docs/conversation-design.md. While she speaks,
BatHandler takes what it hears as her own voice, except speech that
starts with her name or tells her to stop, which cuts her off. There is
no echo cancellation: it needs a reference signal and tuning on real
hardware, and her name is a strong enough signal for now. The follow-up
window opens when she finishes speaking.

## Bella speaks (2026-11-06)

Version 0.130.0. Design: docs/speaking-design.md. A new service,
MynahHandler, says Bella's answers to spoken requests aloud with the
Kokoro engine; typed requests stay silent. It is a separate process so a
long sentence never holds up Bella's loop, as BatHandler is for hearing.
The check listens to what the guest played with the host's Moonshine
engine, so a pass means the words were audible, not only logged.

## Bella's voice: the Kokoro engine (2026-11-06)

Version 0.129.0. Design: docs/speech-design.md. Kokoro-82M runs in
`zoo-infer` from a GGUF that xtask makes from the PyTorch files without
unpickling them (a data-only reader). Checked against an independent
NumPy reference, and by Moonshine hearing it back, since no PyTorch is
installed. Too slow for conversation on one thread (21× real time on
the host): speed comes before Bella speaks for real. Text to phonemes
follows misaki's rules with its Apache-2.0 word lists and no tagger or
espeak-ng; words in no list are split or spelt out.

## Commands that name nothing (2026-11-06)

Version 0.128.0. Design: docs/vague-design.md. When the model's move,
close or remove has only a pronoun, or "somewhere else", for its argument,
Bella answers in words and neither acts nor asks. Typed commands are
unchanged.

## Ending at a pause (2026-11-06)

Version 0.127.0. Design: docs/endpoint-design.md. At 300 ms of quiet
BatHandler writes down the speech so far, if the last utterance shows
that takes at most 400 ms, and takes it when it is for her and sounds
finished. Otherwise the 700 ms end decides as before. Under TCG it is
never tried.

## Answers said quickly (2026-11-06)

Version 0.126.0. Design: docs/late-design.md. BatHandler keeps the last
sentence not for her for 3 s, in memory only, so an answer said before
her question reaches it isn't lost on a slow processor.

## Conversation, step 2: judged by meaning, not used yet (2026-11-06)

Version 0.132.0. Design: docs/addressee-design.md. Letting the model
decide whether speech after her answer is said to her was measured on
34 labelled sentences with Qwen3-1.7B Q4_0, using four methods.

- **Asking the model (yes/no, or computer/person with examples) gives
  the same answer to every sentence:** 16 of 16 sentences of talk taken.
- **Reading the sentence as said to her and as said to someone else,
  and comparing the likelihoods,** is the only method with a signal. It
  still takes 8 of 16 at a margin of 0, and only 5 of 18 said to her
  when strict enough to take no talk. It takes 7–10 s per sentence on the
  host.
- **Not wired into Bella:** a judge that takes talk would put words not
  meant for her into the logs. The code and `cargo xtask
  bella-addressee` stay for the model bake-off, where the number to
  beat is how much said to her is taken with no talk taken.

## One codegen unit for release builds (2026-11-06)

Version 0.132.0. Adding `zoo_infer::addressee`, a module the guest never
calls, made BatHandler hear twice as slowly under TCG: "Hey Bella, open
notes" took 10.2–11.3 s to hear in 5 runs, against 4.9 s on `main`, and
`voice-follow`, `voice-follow-chat` and `voice-confirm` timed out. With
the module left out of the guest's build it took 4.7 s. Release builds
had Cargo's default of 16 codegen units, so the module changed how
zoo-infer was split, and with it what was inlined into the hearing
code.

- **`codegen-units = 1` in `[profile.release]`** (the kernel and the
  guest's programs): with the module back in, 5.1 s, and the case
  passes. Speed no longer depends on how the crate happens to be split.
- Host tools are unaffected: they build zoo-infer in the dev profile.

## Titled notes (2026-11-06)

Version 0.133.0, docs/titled-design.md. "Add passport to my packing
list" used to go to the one note, `Documents/notes.txt`.

- **A titled note is a file, `Documents/Notes/<Title>.txt`,** not a
  section inside `notes.txt`: Files shows and opens it like any other
  file, and two notes can never overwrite each other's lines. The title
  is letters, digits, spaces and `-`, at most 40 bytes, so it can't name
  a path outside the folder.
- **The title is read by a pattern, before routing**
  (`zoo_infer::notes::titled_place`), and the place is put back the way
  the registry spells it, so the registry, the model and the regression
  baseline see the request they always saw. It is not left to the model,
  so the title is exactly the words said, it costs no model time, and
  `bella-regress` checks it.
- **Carried as an extra part, `^<title>`,** beside `#<id>` and `@<line>`,
  so the window server's slot check is unchanged.
- **Only a write makes a note.** A remove or change in a note that
  doesn't exist is an error, not an empty new file. With several notes
  matching, Notes changes nothing and names them.

## Running verify cases at once (2026-11-08)

Version 0.134.0, docs/parallel-design.md. `cargo xtask verify --jobs N`
(1 to 8) runs cases on N workers; the default stays 1.

- **Each case is still its own machine;** only the boot image is shared,
  opened with `snapshot=on` so no case writes it.
- **Cases on one shared disk stay in order on one worker,** and cases
  whose checks bound a time, or that share microphone files, run alone
  after the rest, so `--jobs` changes no case's bound.
- **QEMU settings became thread-local** rather than passed through every
  call, the smallest change to the existing harness.
- Measured: TCG 32.1 → 14.0 min with 4 jobs, WHPX 49.8 → 22.4 min with 3
  (docs/verification.md). Whether the everyday command uses `--jobs` is
  the owner's call.

## Qwen3.5-2B: ported, measured, not Bella's model (2026-11-08)

Version 0.135.0, docs/qwen35-design.md. The first model of the owner's
bake-off (approved 11-06).

- **Converted from the official BF16 weights** (`cargo xtask
  qwen35-gguf`), not taken from a third party's GGUF: Qwen publishes none.
- **The engine gains Gated DeltaNet and gated attention** rather than a
  second engine: both kinds of layer share the projections, norms and
  feed-forward code, and Qwen2/Qwen3 run the same arithmetic as before.
- **The recurrent state is snapshotted, not recomputed,** for Bella's two
  rewinds (end of the system prompt, end of the conversation): 18 MiB a
  copy, against reading the system prompt again for every request.
- **Checked against our own NumPy reference** with the same `Q8_0`
  weights (`cargo xtask qwen35-check`), as Qwen2.5 was.
- **Qwen3-1.7B stays Bella's model**: Qwen3.5-2B answered 38 of 46
  `assist-check` requests right against 44, and is better at judging
  who speech is for (12 of 18 with no talk taken, against 5) but still
  short of the bar, at 8.7 s a sentence. Gemma 4 E2B is not ported.
