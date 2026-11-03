# Stage 2: model lifecycle and responsiveness

Status: in progress (2026-11-02). Part of the Bella roadmap
(docs/bella-roadmap.md). Goal: Bella starts warm, survives its services
failing, and never stalls the desktop.

| Item | Status |
| --- | --- |
| B12: scheduler priorities; the model runs in the background | done, version 0.79.0 |
| Warm the system prompt in the background at desktop start | done, version 0.80.0 |
| The model on its own disk | done, version 0.81.0 |
| B10: disk requests off the scheduler lock | measured: 52 µs per request; not changed (§3) |
| Restart ElephantHandler and OtterHandler with back-off | open |
| B14: non-blocking broker calls | open |
| B13: a bounded kernel heap per process | open |
| The broker's rights narrowed; audit lines | open |

## 1. B12: background programs

**The problem** (measured in Stage 1, docs/s1-design.md §4):
- The scheduler was plain round-robin on one CPU.
- While the model computed, a pointer move waited for the model's 10 ms
  tick to end before PeacockHandler read it: 4.7 ms on average, 9.6 ms at
  worst.

**The design:**
- **New system call `SET_BACKGROUND` (75):** a program marks itself
  background work. `zoo_rt::set_background(true)` sets
  `Proc::background`. ElephantHandler calls it when it starts as Bella's
  model service.
- **`pick_next` prefers ready tasks of other programs.** A background task
  runs only when nothing else is ready, apart from the kernel task idling.
- **No starvation.** A background task that has not run for
  `BACKGROUND_STARVE` ticks (1 s) gets the next turn even when others are
  ready. Each task records `last_ran` when it is switched in.
- **Input preempts at once.** When a keyboard or pointer interrupt wakes a
  waiting task while a background task runs, `on_input` switches away at
  once instead of at the next tick.

**Why not general priority levels:** one flag covers the case that
exists: inference, the only long computation, against everything
interactive. Levels can come with SMP (Stage 6) if needed.

**Measured** (`responsiveness`, `--accel whpx`, pointer moves while the
model reads its system prompt):

| | Before (Stage 1) | After |
| --- | --- | --- |
| Input wait, mean | 15,051,414 cycles (472‰ of a tick, 4.7 ms) | 825,412 cycles (25‰, 0.25 ms); a second run: 18‰ |
| Input wait, longest | 958‰ (9.6 ms) | 598‰ (6 ms); one event of 40 |
| First request, Enter to answer | 6,011 ms | 6,103 to 6,290 ms |
| Second request, Enter to answer | 1,557 ms | 1,600 ms |

- **Input waits about 19 times less.**
- **The model is about 2-5% slower,** because the desktop now goes first
  while the pointer moves.
- **One event in 40 still waited about 6 ms.** It probably arrived while
  PeacockHandler was blocked on something other than input (a channel),
  so it was not an input waiter to wake. Not investigated further.

**The check:** `responsiveness` now requires the busy-time mean to be at
most 150‰ (1.5 ms) over at least 20 events.
- **Negative control:** ElephantHandler not marking itself background →
  `FAIL` (`tick: mean 481 outside 0..=150`).

## 2. The warm-up

**The problem:** Bella started the model service on the first request in
words. That request then paid for reading the 76-token system prompt:
4.7 s of a 6.0 s answer, against about 1.5 s for later requests.

**The design:**
- **Bella starts ElephantHandler as it opens** and sends `warm`, when the
  model is in the initial filesystem. An image without the model behaves
  as before.
- **ElephantHandler reads the system prompt then**, as a background
  program (§1), so the desktop does not wait for it. It logs `warmed up:
  system prompt N tokens in T ms`.
- **A request arriving during the warm-up** is kept and answered when the
  warm-up ends. It is told `busy setup done/total` at once, and at every
  later step, so Bella shows progress.
- **A `cancel` during the warm-up** drops the kept request and answers
  `err cancelled`. The warm-up goes on.
- **Without the model,** ElephantHandler ignores `warm`: it expects no
  answer.

**Process order:** the model service is now started before the file
broker (pid 31, then 32, in the desktop's test cases with the model).

**Measured** (`--accel whpx`):

| | Before | After |
| --- | --- | --- |
| First request, Enter to answer (`bella-live`, asked after the warm-up) | 6,103 ms | **1,042 to 1,082 ms** |
| Second request | 1,600 ms | 1,619 ms |
| Warm-up at desktop start | — | 4.1 to 5.8 s, in the background |
| Request arriving during the warm-up (`everyday-model`) | 6,002 ms | 3,186 ms |
| Input wait during the warm-up, mean (`responsiveness`) | — | 1‰ of a tick, the same as idle |

The first request is now about as fast as later ones, as the roadmap's
"done when" asks.

**Checks:**
- **`bella-live`** waits for `warmed up`, then requires the first answer
  within 3,000 ms.
- **`responsiveness`** now moves the pointer during the warm-up and
  requires the warm-up to end after the figures, so the model was busy
  throughout. It then measures idle.
- **`bella-model`** (TCG; its request arrives during the warm-up) still
  shows `Thinking:` and still cancels.

**Negative controls:**
- **Bella not sending `warm`:** `FAIL bella-live`. Its expectation already
  fails on the model service starting late.
- **ElephantHandler not background, with the warm-up:**
  `FAIL responsiveness` (`tick: mean` 501).

## 3. The model on its own disk

The owner's choice ("model loads from disk"). A3 put the model in the
everyday system's boot image, where it cost 1.85 s of boot (the boot
loader reads 411 MB more) and 411 MB of the boot image.

**Why a read-only disk of its own, not the data disk:**
- The data disk is the home folder's ZooFS 2 (docs/linux10c-design.md).
  Putting the model in it means writing a 411 MB file into the user's
  file system from the host, or a first-boot copy.
- **A separate read-only disk** is a plain file the host writes once, that
  the guest cannot damage (`readonly=on`), and that is separate from the
  owner's data.
- It is what a system partition will be on real disks.

**The disk** (`build/model-disk.img`, `image::model_disk`):
- **Layout:** block 0 is a header, `ZOOMODL1` then the model's length (8
  bytes, little-endian); the model follows from block 1, padded to whole
  4 KiB blocks.
- **Written once:** only when the model file is newer or a different size,
  and only after its pinned SHA-256 checks.
- **Attached** as a second legacy `virtio-blk-pci` drive with
  `readonly=on`.

**The kernel:**
- **Finding the disks:** `virtio_blk::init` sets up every legacy virtio
  block device (up to two). One whose block 0 starts with `ZOOMODL1` is
  the model disk; the other is the data disk. A model length that would
  not fit the disk is refused.
- **New system calls:** `MODEL_SIZE` (76) and `MODEL_READ` (77) read up to
  16 blocks per call. Any program may read: the model is public,
  read-only data.

**ElephantHandler:**
- **Where the model comes from:** the initial filesystem, if the boot image
  has it (the USB stick, the test images); else the model disk.
- **Reading it:** it reads the whole model into page-aligned memory, as a
  background program, and logs `read the model from the model disk: N MB
  in T ms`.
- **Bella** warms the model up when it is in either place.

**`cargo xtask run`** now boots the small fast-boot image plus the model
disk. `--no-model` leaves the disk out. **`usb-image`** keeps the model
in the boot image, because ZooOS has no USB storage driver yet (Stage 7).

**Measured** (`everyday-model`, `--accel whpx`, from QEMU starting, in
`serial-times.log`):

| | Model in the boot image (A3) | Model on its disk |
| --- | --- | --- |
| Kernel up (`BOOT`) | 2,753 ms | **1,035 ms** |
| Desktop up (Bella started) | 2,930 ms | **1,210 ms** |
| Model read into memory | — | 341 ms (408 MB, about 1.2 GB/s), done at 2,355 ms |

- **The boot is 1.72 s faster.**
- **Bella's model now sits in ElephantHandler's heap** (437 MB peak), not
  in the initrd: the same memory while Bella runs, none before.

**B10, measured rather than changed:**
- **The cost:** 408 MB in 341 ms is about 6,500 requests of 64 KiB, so
  about 52 µs each. Each request polls the device with interrupts off
  and the scheduler lock held: for that long, nothing else runs.
- **Why that is small:** 52 µs is 0.5% of a 10 ms tick, and the requests
  come from a background program.
- **Why it waits:** interrupt-driven, unlocked disk requests are left
  until a slower disk (a real one, Stage 7) makes them matter.

**Checks:**
- **`everyday-model`** boots the fast-boot image plus the model disk, as
  `run` does, and requires `read the model from the model disk` and the
  answer.
  - **Negative control:** no model disk attached → `FAIL` (no model, so
    no model service starts).
- **`bella-live`** (the model in the boot image): still `OK`, first answer
  1,082 ms.
