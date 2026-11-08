# Running verify cases at once (2026-11-06)

Status: implemented (version 0.134.0). `cargo xtask verify --jobs N` runs
up to N cases at the same time (1 to 8; the default is 1, as before). The
suites had grown to about 35 minutes under TCG and 45 under WHPX, almost
all of it guests booting one after another while most of the host's 24
processors sat idle.

## 1. What runs together

- **Every case is its own machine.** Its run folder (`build/runs/<case>/`,
  with its own copy of the firmware variables), its data disk
  (`build/disks/<case>.img`) and its QMP port are its own already.
- **The boot images are shared.** Several QEMUs open the same
  `zoo-os-<mode>-uefi.img`, so with `--jobs` above 1 each boots it with
  `snapshot=on`: writes go to a temporary overlay, never to the image.
  With `--jobs 1` the command line is unchanged.
- **Cases on one shared disk run in order on one worker.** `notes-1` and
  `notes-2`, `persist-1` to `persist-5`, `wine-home-1` and `wine-home-2`
  are one machine booted several times (`Disk::Shared`), so they are one
  group (`plan_groups`, unit-tested).
- **Some cases run alone, after the rest** (`runs_alone`): the ones whose
  checks bound a time (`bella-live`'s 3000 ms, `responsiveness`, the
  `animation` frame times, `smp-` cases), and the `voice-`, `mic-`,
  `hear-` and `audio-` cases, whose microphone files are shared between
  cases and whose hearing is timed. `usb-model` builds a 1.7 GB image of
  its own, so it runs alone too.
- **QEMU settings are per worker.** `qemu::set_cpus`, `set_mic` and the
  rest were process-wide; they are thread-local now, so each worker's
  next machine gets its own case's settings.

The report keeps the cases' order; the progress lines of cases running
at once interleave.

## 2. What it found

With 3 at once, five cases failed, the same way every time:
`dock-magnify`, `dock-bounce`, `notes-1`, `notes-cursor` and `notes-2`
(and `dock-menu` with 4). Each reveals the auto-hidden dock by moving the
pointer to the bottom edge, waited a fixed 500 ms (`DOCK_SLIDE`) for it to
slide in, and then pointed at an icon. With several guests on the host the
slide takes longer, so the click landed on the dock before it was there:
nothing opens. The cases now wait until the latest
`PeacockHandler: dock` line says `shown` (`dock_shown()`, an
`Action::WaitLatest`) instead of the fixed time. Waiting for any `dock
shown` line was not enough: when the dock is still in from an earlier
reveal, that line is already in the log. This also makes the cases
sturdier one at a time on a busy host.

## 3. Measured

See docs/verification.md, "Running verify cases at once".

## 4. Not done

- `verify` still defaults to one at a time; the owner decides whether the
  everyday command should use `--jobs`.
- The output of cases running together is interleaved; each case's own
  lines are in its `serial.log`.
