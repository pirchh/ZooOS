# Verification record

Status: M0–M5 (M5 as a prototype) and the interim console verified on the
host below on 2026-10-30. Logs and screenshots are generated under `build/`
(ignored by Git); rerun the commands to reproduce them. The screen
section (version 0.88.0) is the current state. Sections before it
quote the program names in use at the time (for example LionHandler,
OwlHandler).

**The 0.3.0, M2 and M1 sections were recorded before the rename and quote the
output exactly as it was then**, so they show `ORIGINS_OS` markers,
`origins-os` image names and the `origins>` prompt. Paths containing
`\OriginsOS\` refer to the local checkout folder, which kept its name.

## Host and versions

| Item | Version |
| --- | --- |
| Host OS | Windows 11 Home 10.0.26200 (x86-64), PowerShell + Git Bash |
| Toolchain | `nightly-2026-10-01` → rustc 1.101.0-nightly (21b707e3f 2026-09-30), cargo 1.101.0-nightly (f3865b2a4 2026-09-29), host `x86_64-pc-windows-msvc` |
| Components | rust-src, llvm-tools, rustfmt, clippy; target `x86_64-unknown-none` |
| Linker (host) | MSVC `link.exe` from the existing Visual Studio Build Tools install |
| Crates (host) | bootloader 0.11.17 (UEFI feature only), sha2 0.11.1 |
| Crates (kernel) | bootloader_api 0.11.17, x86_64 0.15.5, pic8259 0.11.0, linked_list_allocator 0.10.6, spin 0.12.3, noto-sans-mono-bitmap 0.3.2, pc-keyboard 0.9.0 (full set in `Cargo.lock`) |
| Crates (host, since console) | noto-sans-mono-bitmap 0.3.2 (re-renders text to check screenshots) |
| Workspace crates (since M3) | `zoo-abi` (shared ABI, ELF and initrd parsers, no dependencies), `zoo-user` (runtime + programs) |
| User programs (M4) | adds peacock-handler 66,144 B (plus an 8.8 MB zero-filled back buffer in `.bss`), owl-handler 16,336 B, chimp-handler 15,184 B; initrd 151,552 B |
| User programs (M3) | zookeeper 18,496 B; monkey-handler 14,392 B; lion-handler 10,616 B; hamster-handler 3,192 B (static ELF at `0x200000`); initrd 51,200 B |
| QEMU | 11.1.0, firmware as recorded in `tools/firmware/README.md` |
| Kernel ELF | M1: 11,504 bytes. M2: normal 60,512 bytes, fault modes 40–46 KB (release); crate `zoo-os-kernel` since 0.4.0 |

## Exact QEMU invocation (verify cases)

    "C:\Program Files\qemu\qemu-system-x86_64.exe" -machine q35 -accel tcg -cpu qemu64 -smp 1 -m 512M
      -drive if=pflash,format=raw,unit=0,readonly=on,file=<OVMF_CODE>
      -drive if=pflash,format=raw,unit=1,file=build\runs\<case>\OVMF_VARS.fd
      -drive format=raw,media=disk,file=build\<mode>\zoo-os-<mode>-uefi.img
      -nic none -display none -monitor none -no-reboot
      -chardev file,id=com1,path=build\runs\<case>\serial.log -serial chardev:com1

`cargo xtask run` uses the same machine with `-serial stdio` instead of the
chardev. Commas in paths are escaped as `,,`. The exact command for each run
is saved in `build/runs/<case>/qemu-command.txt`.

## Screen results (version 0.88.0)

Design: docs/context-design.md (with the wording table).

**`bella-context`** (TCG): `OK`.

```
[27:peacock-handler] PeacockHandler: context for Bella
[30:bella] Bella: the screen: focused: Bella; open: Files, Crash Test, Bella, Notes (zoo.rs); recently: opened Notes, opened Documents/zoo.rs in Notes
```

**`bella-context-answer`** (`--accel whpx`): `OK`.

```
[31:elephant-handler] ElephantHandler: said `You are working on `zoo.rs`, a file that is currently open in the "Notes" application.` in 2256 ms
```

**Negative control:** no snapshot → `FAIL`, ``said `I am Bella, the
assistant built into ZooOS, an operating system.` ``

**Still `OK`:**
- with WHPX: `bella-facts-answer` (with the new context label), `bella-talk`,
  `bella-convo` and `bella-live`;
- under TCG: `bella-facts`, `bella-access` and `bella`.

**Full suite** (`cargo xtask verify --skip-model`, TCG): `PASS`, 97 cases
`OK` and 10 skipped because they need an accelerator.

## Facts results (version 0.87.0)

Design: docs/facts-design.md.

**`bella-facts`** (TCG): `OK`.

```
[31:otter-handler] OtterHandler: remembered a fact (2 kept)
[30:bella] Bella: from what you told me: my dentist appointment is on Friday
[38:otter-handler] OtterHandler: listed 2 facts
[38:otter-handler] OtterHandler: forgot 1 facts
[38:otter-handler] OtterHandler: listed 1 facts
```

The `listed 2 facts` with pid 38 comes after a desktop restart.

**`bella-facts-answer`** (`--accel whpx`): `OK`.

```
[31:elephant-handler] ElephantHandler: said `Your dentist appointment is on Friday.` in 953 ms
```

**Negative control:** the fact not sent → `FAIL`, ``said `I'm Bella, not a
dentist. I can't provide dental care.` ``

**Still `OK`:**
- with WHPX: `bella-talk`, `bella-convo` and `bella-live`;
- under TCG: `bella`, `bella-memory`, `memory-guard` and `bella-access`.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 96 cases `OK`.
- clippy `-D warnings`: clean, after one fix (`sort_by_key` with
  `Reverse` in `fact_find`; the same order).
- `cargo fmt --check`: clean.
- `zoo_abi` tests: 19 passed.

## Broad access results (version 0.86.0)

Design: docs/access-design.md.

**New case `bella-access`** (TCG, typed commands): `OK`.

```
[31:otter-handler] OtterHandler: info: home holds 6 files, 4 folders
[31:otter-handler] OtterHandler: `notes` is `Documents/notes.txt` (found by name)
[32:files] Files: showing Home/Music
[31:otter-handler] OtterHandler: moved Documents/notes.txt to Music/notes.txt
[31:otter-handler] OtterHandler: moved Music/notes.txt back to Documents/notes.txt
[31:otter-handler] OtterHandler: moved Documents/recipes to the Trash as .Trash/recipes
[30:bella] Bella: launched notes
```

**Negative control:** `trash` without asking → `FAIL bella-access`, `` `to
the Trash as` appeared, and must not `` (after the `n`).

**Two first runs failed, on the test's side and then mine:**
- **The test's side:** `open Music` opens a Files window, which takes the
  keyboard, so the case now clicks Bella's title bar to come back.
- **My side:** `move … to …` was taken for a sentence because "to" is a
  sentence word. For `move` it is now part of the command
  (`zoo_abi::bella` test added).

**`bella-convo`** (WHPX) also failed once more: "is that it?" became
`read file playlist.txt`. Yes/no questions about the last answer now go
straight to a reply in words (docs/talk-design.md §6): `OK`.

**`assist-check`** (host, 31 requests, the chosen prompt): 0.5B 17 of 23 and
2 of 8 (`PASS`); 1.5B 19 of 23 and 5 of 8.

**With the first prompt** (command words only), `bella-live`,
`everyday-model` and `model-restart` failed: "Find my pancake recipe"
became `inspect pancake recipe`. Describing each command fixed it. A
second attempt made `bella-convo` fail: "where is my music" became
``move `/music to Music` `` (refused: the path escapes home). Saying
`search` finds "where files are" fixed that, and the request joined the
set.

**Full `cargo xtask verify --skip-model`** (TCG): 94 cases `OK`, and one
`FAIL`: `bella-model` looked for Bella's old idle line,
`…search/list/inspect/create/undo.`, which this change replaced. With its
`HINT_TEXT` updated, `bella-model` alone: `OK`.
- clippy `-D warnings` and `cargo fmt --check`: clean.
- `zoo_abi` tests: 18 passed.

## Stage 3 results: XSAVE and AVX2 (version 0.85.0)

Design: docs/stage3-design.md (all the tables).

**Host:**
- **`infer-check`** (AVX2): `PASS`, top-5 logits within 0.00005.
- **`assist-check`:** the same answers and margins with AVX2 and with
  `--portable`. A request takes 0.5 s against 1.0 s.
- **Unit test `avx2_agrees_with_portable`:** `OK`.

**Guest** (`--accel whpx`):

```
cpu: xsave=true avx=true register state 576 bytes
[6:narwhal-test] NarwhalTest: AVX registers kept over 12 ticks
[7:narwhal-test] NarwhalTest: AVX registers kept over 12 ticks
[31:elephant-handler] ElephantHandler: service ready (model mapped, parsed in 249 ms, engine avx2)
[31:elephant-handler] ElephantHandler: warmed up: system prompt 76 tokens in 1972 ms
[30:bella] Bella: answered in 520 ms by the model
```

**Negative control:** XSAVE without the AVX component → `FAIL normal`,
`narwhal-test code=5`, `AVX registers changed across task switches`.

**All `OK` with WHPX:**
- `inference` (`tokens match the host engine exactly`);
- `bella-live`, `everyday-model` (also with `--bella 1.5b`), `bella-talk`
  and `responsiveness`;
- `wine-cmd`, `wine-version`, `wine-home-1` and `wine-home-2`;
- `linux-hello`, `-eider`, `-skua`, `-fulmar`, `-guillemot`, `-kittiwake`,
  `-ermine`, `-loon`, `-auklet`, `-dovekie` and `-redpoll`.

**Tried first:** `-cpu host` under WHPX: OVMF `#GP` in PlatformPei.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 94 cases `OK`.
Under TCG the kernel logs `cpu: xsave=false avx=false register state 512
bytes` (plain `qemu64`), so the FXSAVE fallback and the portable engine
are what the suite runs.
- clippy `-D warnings` (kernel, `zoo-user`, `xtask`, `zoo-infer`): clean.
- `cargo fmt --check`: clean.
- Tests: `zoo-infer` 12 and `xtask` 25 passed.

## Bella keeps the conversation (version 0.84.0)

Design: docs/talk-design.md §6.

**New case `bella-convo`** (`--accel whpx`): `OK`.

```
[31:elephant-handler] ElephantHandler: chose search `music`
[31:elephant-handler] ElephantHandler: noted an exchange in 1903 ms (37 tokens of conversation)
[30:bella] Bella: a follow-up; with the last exchange
[31:elephant-handler] ElephantHandler: said `Yes, that's the information I found for you. If you have any other questions or need further assistance, feel free to ask.` in 2630 ms
```

**Negative control:** no follow-up context → `FAIL bella-convo`; the model
chose ``list `.` `` again.

**Still `OK`** (WHPX): `bella-talk`, and `bella-live` (first answer
1,080 ms).

**Host tests:** `zoo_abi` 17 passed (new `follow_ups`).

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 94 cases `OK`.
clippy `-D warnings` and `cargo fmt --check`: clean.

## Bella talks; Qwen2.5-1.5B (version 0.83.0)

Design: docs/talk-design.md (with the 0.5B and 1.5B measurements).

**`bella-talk`** (`--accel whpx`): `OK`.

```
[31:elephant-handler] ElephantHandler: warmed up: reply prompt 61 tokens in 3102 ms
[31:elephant-handler] ElephantHandler: said `My name is Bella, the assistant built into ZooOS.` in 1333 ms
[30:bella] Bella: no command fits
[31:elephant-handler] ElephantHandler: said `Why did the tomato turn red? Because it saw the salad dressing!` in 1486 ms
[31:elephant-handler] ElephantHandler: chose search `my music`
```

**`everyday-model --bella 1.5b`** (`--accel whpx`): `OK`.

```
model disk: 1066227232 bytes of model
[10:elephant-handler] ElephantHandler: read the model from the model disk: 1016 MB in 743 ms
[10:elephant-handler] ElephantHandler: warmed up: system prompt 76 tokens in 17591 ms
[10:elephant-handler] ElephantHandler: timing: setup 0 ms, reading 2712 ms (10 tokens), writing 946 ms (3 tokens), total 3658 ms; heap 1065908 KiB, peak 1065909 KiB
```

**`cargo xtask assist-check --bella 1.5b`** (host): `PASS`, 11 of 14 right,
2 of 8 acted on.

**Host tests:** `zoo_abi` 16 passed (new routing tests), `zoo_infer` 11
passed (new `Q6_K` block).

**Still `OK`:**
- with WHPX: `bella-live` (first answer 1,102 ms), `everyday-model`,
  `model-restart` and `responsiveness`;
- under TCG: `bella`, `bella-memory`, `memory-guard` and
  `bella-any-case`.

**A first run of `bella-live` failed** (`answered in 4021 outside
0..=3000`): the reply prompt's warm-up held the first request back. A
request now interrupts that warm-up.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 94 cases `OK`.
clippy `-D warnings` (`zoo-user`, `xtask`, `zoo-infer`, `zoo-abi`) and
`cargo fmt --check`: clean.

## Stage 2 results: Bella's helpers start again (version 0.82.0)

Design: docs/stage2-design.md §4.

**`broker-restart`** (TCG): `OK`.

```
ZOO_OS PROC killed pid=31 name=otter-handler by=0
[30:bella] Bella: the file broker stopped; a new one in 500 ms
[32:otter-handler] OtterHandler: `music` is `Music`
```

**`model-restart`** (`--accel whpx`): `OK`.

```
ZOO_OS PROC killed pid=31 name=elephant-handler by=0
[30:bella] Bella: the model service ended; starting it again in 500 ms
[32:elephant-handler] ElephantHandler: warmed up: system prompt 76 tokens in 4784 ms
[30:bella] Bella: answered in 1043 ms by the model
```

**Negative controls:**
- **No new broker after a failure:** `FAIL broker-restart`.
- **No restart of the model service:** `FAIL model-restart`.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 94 cases `OK`,
with the new `broker-restart`. `model-restart` was run with `--accel whpx`.
clippy `-D warnings` (kernel, `zoo-user`, `xtask`) and `cargo fmt --check`:
clean.

## Stage 2 results: the model on its own disk (version 0.81.0)

Design: docs/stage2-design.md §3.

**`everyday-model`** (`--accel whpx`, the fast-boot image plus
`build/model-disk.img`): `OK`.

```
model disk: 428730208 bytes of model
[10:elephant-handler] ElephantHandler: read the model from the model disk: 408 MB in 341 ms
[10:elephant-handler] ElephantHandler: warmed up: system prompt 76 tokens in 5499 ms
[9:bella] Bella: answered in 3142 ms by the model
```

The request arrived during the warm-up. From `serial-times.log`:
- `BOOT` at 1,035 ms (A3 image: 2,753 ms);
- Bella started at 1,210 ms (2,930 ms).

**Negative control:** no model disk attached → `FAIL everyday-model`, no
model service started.

**`bella-live`** (model in the boot image): `OK`, first answer 1,082 ms.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 93 cases `OK`,
including `inference` and `bella-model` (the model image). The owner also
ran `cargo xtask run --accel whpx` on this build:
`read the model from the model disk: 408 MB in 703 ms`, then `warmed up`.
- clippy `-D warnings` (kernel, `zoo-user`, `xtask`): clean.
- `cargo fmt --check`: clean.
- `cargo test -p xtask`: 25 passed.

## Stage 2 results: the warm-up (version 0.80.0)

Design: docs/stage2-design.md §2.

With `--accel whpx`:
- **`bella-live`:** `OK`, ``[verify] in range: [30:bella] Bella: answered in
  1042 ms by the model``, the first request after `warmed up: system
  prompt 76 tokens in 4088 ms`.
- **`everyday-model`:** `OK`. The request arrives during the warm-up and is
  answered in 3,186 ms.
- **`responsiveness`:** `OK`. During the warm-up, `input wait 40 events
  mean 55060 … per mille of a tick: mean 1 longest 3`.

Under TCG, with the model image: **`bella-model`** `OK`. `Thinking:` is
shown and the request is cancelled during the warm-up.

**Negative controls:**
- **No `warm`:** `FAIL bella-live`, the model service started late.
- **The warm-up not in the background:** `FAIL responsiveness`
  (`tick: mean` 501).

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 93 cases `OK`.
clippy `-D warnings` (`zoo-user`, `xtask`) and `cargo fmt --check`: clean.

## Stage 2 results: background programs (version 0.79.0)

Design: docs/stage2-design.md (B12).

**`responsiveness`** (`--accel whpx`): `OK`, now with a bound on the
input wait while the model runs:

```
[verify] in range: [27:peacock-handler] PeacockHandler: input wait 40 events mean 588959 max 21252914 cycles; per mille of a tick: mean 18 longest 666
```

**Negative control:** ElephantHandler not marking itself background:

```
FAIL  responsiveness  … `tick: mean` 481 outside 0..=150 in `… input wait 40 events mean 15358729 max 30597298 cycles; per mille of a tick: mean 481 longest 960`
```

`bella-live` (`--accel whpx`): `OK`. First request 6,103 ms Enter to
answer (Stage 1: 6,011 ms), second 1,600 ms (1,557 ms).

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 93 cases `OK`.
The scheduler change touches every program, so the accelerator-only
cases were also run with `--accel whpx`:
- `everyday-model`, `bella-live` and `responsiveness`: `OK`;
- `wine-home-1` (74.8 s) and `wine-home-2`: `OK`.

clippy `-D warnings` (kernel, `zoo-user`, `xtask`) and `cargo fmt --check`:
clean.

## Stage 1 results: "none of these" and timing (version 0.78.0)

Design: docs/stage1-design.md (with the prompt comparison and the baselines).

**`cargo xtask assist-check`** (host, 22 requests):

```
accuracy: 10 of 14 requests with a command answered right; 2 of 8 requests without one acted on anyway
within the recorded bar (at least 10 right, at most 2 acted on)
assist-check: PASS
```

Negative control, `none` never chosen: `8 of 8 … acted on anyway`,
`assist-check: FAIL`.

**In the guest** (`--accel whpx`): `bella-live` and `everyday-model`
`OK`. Both now also require the timing lines. From `bella-live`:

```
[32:elephant-handler] ElephantHandler: service ready (model mapped, parsed in 248 ms)
[32:elephant-handler] ElephantHandler: timing: setup 4720 ms, reading 729 ms (10 tokens), writing 254 ms (3 tokens), total 5703 ms; heap 18579 KiB, peak 18580 KiB
[30:bella] Bella: answered in 6011 ms by the model
[32:elephant-handler] ElephantHandler: timing: setup 0 ms, reading 1032 ms (15 tokens), writing 499 ms (6 tokens), total 1532 ms; heap 18579 KiB, peak 18580 KiB
[30:bella] Bella: answered in 1557 ms by the model
```

**Under TCG:** `bella-any-case` requires `ms by a typed command`, and
logs `answered in 6 ms` and `1 ms`.

**New case `responsiveness`** (`--accel whpx`): `OK`. F11 figures, idle
then during the model's set-up:

```
[27:peacock-handler] PeacockHandler: input wait 40 events mean 56831 max 160317 cycles; per mille of a tick: mean 1 longest 5
[27:peacock-handler] PeacockHandler: input wait 40 events mean 15051414 max 30541159 cycles; per mille of a tick: mean 472 longest 958
```

This is the baseline before scheduler priorities (Stage 2).

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 93 cases `OK`.
The accelerator-only cases were skipped under TCG; `bella-live`,
`everyday-model` and `responsiveness` were run separately with
`--accel whpx`, all `OK`.
- clippy with `-D warnings` (`zoo-user`, `xtask`, `zoo-infer`): clean.
- `cargo fmt --check`: clean.
- Tests: `cargo test -p xtask` 25 passed, `-p zoo-infer` 10 passed.

## Bella rename results (version 0.77.0)

The assistant is Bella everywhere in the OS (DECISIONS.md).

**Renamed cases**, all `OK`: `bella`, `bella-memory`, `bella-any-case`.

**`memory-guard`** now plants its line in `.assistant-memory`, the name
used before. It also requires `OtterHandler: moved .assistant-memory to
.bella-memory`, and the planted `undo` is still ignored after the move:
`OK`.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 93 cases `OK`.
`bella-live` and `everyday-model`, run separately with `--accel whpx`:
both `OK` with the renamed program.
- clippy with `-D warnings` (`zoo-user`, `xtask`): clean.
- `cargo fmt --check`: clean.
- `cargo test -p xtask`: 25 passed.

## Stage 0 results: restarts (version 0.76.0)

Design: docs/s0-design.md (B5, B15).

| Check | Result | Negative control (the fix removed) |
| --- | --- | --- |
| `services`, extended: after `restart badger-handler`, wait for the reset | `OK`: `badger-handler ran 30 s without failing; restart count reset`, then `restarts=0 last=killed` | No reset: `FAIL services` (timeout) |
| `home-refresh` (new): BadgerHandler restarted while the desktop runs | `OK`: `sent the desktop session a fresh home folder`, `reloading the desktop`, then `Files: showing Home/Documents` in the new desktop | No fresh folder sent: `FAIL home-refresh` (timeout waiting for it) |

**The original symptom:** with the fix removed and the test's waits for
the reload removed too, the log shows:

```
[28:files] Files: cannot list `Documents`: PEERCLOSED
```

Files still logs `showing Home/Documents` after a failed listing, so the
case now also requires that `Files: cannot list` never appears.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 93 cases `OK`.
The four accelerator-only cases were skipped.
- clippy with `-D warnings` (kernel, `zoo-user`, `xtask`): clean.
- `cargo fmt --check`: clean.

## Stage 0 results: the memory (version 0.75.0)

Design: docs/s0-design.md (B3, B4, B8).

**New case `memory-guard`:** `OK`. Its log:

```
[30:assistant] Assistant: ignored remembered `undo`
[30:assistant] Assistant: learned `make a new project folder for work called Invoices` -> `create Invoices`
[31:otter-handler] OtterHandler: `create Invoices` names other things than `make a new project folder for work called Receipts`
```

The test also checks two things that must not happen:
- ``learned `find my tax papers` `` never appears, although `search
  taxes` ran (it found nothing);
- `Assistant: recalled` never appears.

| Fix removed (negative control) | Result |
| --- | --- |
| B4: no `recallable` check on recall | `FAIL memory-guard`; the log shows ``Assistant: recalled `undo` (100% alike)``: the planted line ran |
| B3: `none` treated as `ok` | `FAIL memory-guard` (harness error): ``Assistant: learned `find my tax papers` `` appeared |
| B8: no `names_agree` | `FAIL memory-guard`; the log shows ``Assistant: recalled `create Invoices` (75% alike)`` for Receipts |

The first B8 test used "make a folder called Receipts", which is only 60%
alike to the Invoices request. That is below the 75% recall threshold, so
the old code would not have recalled it either. The test now uses a pair
that is 75% alike.

`assistant-memory` (the A2 case) still passes.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 92 cases `OK`.
The four accelerator-only cases were skipped.
- clippy with `-D warnings` (kernel, `zoo-user`, `xtask`): clean.
- `cargo fmt --check`: clean.

## Stage 0 results: filesystem and Notes fixes (version 0.74.0)

Design: docs/s0-design.md (B1, B2, B6, B7 of docs/bella-audit.md).

| Check | Result | Negative control (the fix removed) |
| --- | --- | --- |
| `notes-large` (B1) | `OK`: `opened Music/liner-notes.txt (9230 bytes)`, `saved 9234 bytes` | Old behaviour (8 KiB, no read-only guard): `FAIL notes-large`, Notes logged `opened Music/liner-notes.txt (8192 bytes)` |
| ArcticFoxTest 48 (B2), in `normal` | `OK` | `FAIL normal`: `arcticfox-test code=48`, ``check 48 failed: a removed folder's handle creates nothing`` |
| ArcticFoxTest 49 (B6) | `OK` | `FAIL normal`: `crash pid=8 name=badger-handler fault=page-fault access=write`, which reproduces the audit's finding |
| ArcticFoxTest 50 (B7) | `OK`: `pid 9 holds 31 folder channels; refused another` after 29 new ones | `FAIL normal`: `code=50`, after `opened 56 folder channels` |

With the limit lowered to 8 KiB but the guard kept, Notes opened the file
read-only (`is read-only: over 1 MiB, too large to edit`). That shows the
guard works at any limit.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 91 cases `OK`.
`assistant-live` and `everyday-model` are skipped under TCG.
`wine-home-1` and `wine-home-2`, which open many folders, were run
separately with `--accel whpx`: both `OK`, so the per-process limit leaves
Wine room.
- clippy with `-D warnings` (kernel, `zoo-user`, `xtask`): clean.
- `cargo fmt --check`: clean.
- `cargo test -p xtask`: 25 passed.

## Assistant step 2b results: names in any letter case (version 0.73.1)

The owner asked "where is my music"; the model answered `list music`,
which failed because the folder is `Music`.

**New case `assistant-any-case`:** `OK`.
- `list music` typed in the Assistant → ``OtterHandler: `music` is
  `Music` `` in the log, and the screenshot shows
  `  file playlist.txt (46 bytes)`.
- `list musik` still shows `Error: not found`.

**Negative control:** `list` without the case matching → `FAIL
assistant-any-case` (timeout: the log line never came).

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 90 cases `OK`.
`wine-home-1`, `wine-home-2`, `assistant-live` and `everyday-model` are
skipped under TCG, as before.

## Assistant step 3 results: the model in the everyday system (version 0.73.0)

Design: docs/a3-design.md (with the boot-time table).

**New case `everyday-model`** (`--accel whpx`): `OK`.
- The fast-boot image with the model (52 initrd files, 411 MB) starts in
  the desktop.
- `Find my pancake recipe` →
  ``[11:elephant-handler] ElephantHandler: chose search `pancake recipe` ``.

**Boot cost** (`--accel whpx`, `serial-times.log`):

| | Kernel up | Desktop up |
| --- | --- | --- |
| Without the model | 918 ms | 1080 ms |
| With the model | 2753 ms | 2930 ms |

The first request then took 3.44 s for the model's system prompt and about
1 s more for the answer.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 89 cases `OK`.
`everyday-model`, `assistant-live` and the two `wine-home` cases are
skipped under TCG; the first two pass with `--accel whpx`.
`cargo test -p xtask` (25) and clippy on `xtask`: clean.

## UI step 3h results: Rename and the Trash (version 0.72.0)

Design: docs/u3h-design.md.

**New case `files-manage`** (TCG): `OK`.
- `renamed Documents/notes.txt to Documents/shopping.txt`;
- `moved Documents/recipes to the Trash`;
- `showing Home/.Trash`, with `1-trash` showing the location `Trash` and
  `recipes` selected;
- `put recipes back in Home`.

**Negative control:** Move to Trash that does nothing → `FAIL
files-manage` (timeout).

**A test race, fixed in the test:** the first run's keys reached Files
before the menu's answer (the window server takes the keyboard before the
mouse in a pass), so the rename hit `Documents`
(`renamed Documents to Doshopping.txt`). The test now waits for
`renaming notes.txt` before typing.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 89 cases `OK`.
`assistant-live` passes with `--accel whpx` at `ZOO_OS BOOT
version=0.72.0`. `cargo test -p xtask` (25), clippy on `zoo-user` and
`xtask`, and `cargo fmt --check`: clean.

## UI steps 4b and 2h results: kept appearance, word wrap, launch bounce (version 0.71.0)

Designs: docs/u4b-design.md, docs/u2h-design.md.

| Case | Result |
| --- | --- |
| `appearance-saved` (new) | `OK`: F8 → `appearance saved`; after a desktop restart `[32:peacock-handler] … appearance dark`; Files' title in dark colours |
| `finder` (updated) | `OK`: `"Badger"` found whole in the string colour, the long line now wrapping between words |
| `dock-bounce` (new) | `OK`: Notes opened from the dock with Shift; about 1.2 s in, its yellow band 14 px above the icon's resting top; `animation bounce Notes: frames 46 time 6403 ms` |

**Negative controls**, each run once and then reverted:

| Broken on purpose | Result |
| --- | --- |
| `.appearance` never read | `FAIL appearance-saved` (timeout) |
| The old mid-word wrap | ``FAIL finder: … `"Badger"` not on screen`` |
| No hop drawn | ``FAIL dock-bounce: … 0 yellow pixels on row 992`` |

The first `dock-bounce` run caught the icon in the air, but its sample row
fell on the thin line under the yellow band, so the row was moved up (the
pixel column was read to choose it).

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 88 cases `OK`
(87 before the bounce, then 88 with it). `assistant-live` passes with
`--accel whpx` at `ZOO_OS BOOT version=0.71.0`. `cargo test -p xtask`
(25), clippy on `zoo-user` and `xtask`, and `cargo fmt --check`: clean.

## UI step 4a results: menus that open (version 0.70.0)

Design: docs/u4a-design.md.

**New case `menu-bar`** (TCG): `OK`.
- `menu bar Window: Minimize, Zoom, Files, Crash Test, Assistant`.
- `1-window-menu`: `Minimize` (234, 51) with `Ctrl+M` (372, 51) on its
  line, `Crash Test` (250, 132), `Assistant` highlighted (250, 156).
- Then `brought Assistant forward`; File, then slid to View
  (`menu bar View: Dark Appearance`); `appearance dark`; Ctrl+M minimised
  the Assistant.
- `2-after`: dark titles, no Assistant title, `Files` focused.

**Negative control:** titles that ignore clicks → `FAIL menu-bar`
(timeout).

**First-run fixes:**
- the ✓ printed as `???` in the ASCII log, so the log leaves it out;
- space-indented labels sat at fractional positions the check could not
  match, so the ✓ has its own column.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 86 cases `OK`.
`assistant-live` passes with `--accel whpx` at `ZOO_OS BOOT
version=0.70.0`. `cargo test -p xtask` (25), clippy on `zoo-user` and
`xtask`, and `cargo fmt --check`: clean.

## UI step 2g results: dock magnification (version 0.69.0)

Design: docs/u2g-design.md.

**New case `dock-magnify`** (TCG): `OK`.
- `1-magnified`: the pointer on Crash Test's icon; its tile reaches 24 px
  above the dock's top edge, as designed (84 − 52 − 8).
- `2-settled`: the pointer at the bottom edge; the tile is back below the
  edge and the running dots are at their resting places.

**Negative control:** magnification off → ``FAIL dock-magnify: … reaches
only 0 px above the dock``.

**Updated case:** `dock`'s hover screenshots count running dots and look
for one under the pointer (magnified neighbours move; the first try with
fixed positions failed on exactly that).

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 85 cases `OK`.
`assistant-live` passes with `--accel whpx` at `ZOO_OS BOOT
version=0.69.0`. `cargo test -p xtask` (25), clippy on `zoo-user` and
`xtask`, and `cargo fmt --check`: clean.

## Network step 3 results: the Network app and DNS (version 0.68.0)

Design: docs/n3-design.md.

**New case `net-app`** (TCG): `OK`.
- Files → Applications → Network.
- `Network: connected 10.0.2.15 via 10.0.2.2`.
- `10.0.2.100` typed → ``fetched http://10.0.2.100/ (10.0.2.100): 92
  bytes, `HTTP/1.0 200 OK` ``.
- `1-fetched`: `Network` focused, `Connected  10.0.2.15` in the function
  colour, and `Hello from the host, ZooOS!` in the window.

**Negative control:** no server behind 10.0.2.100 → `FAIL net-app` (the
fetch never succeeds; timeout).

**A bug found while testing.** The first runs fetched 0 bytes, and the
server logged a connection that sent nothing. QEMU's `guestfwd` target is
a character device that QEMU connects when it starts, and the server gave
up after 5 s, before the desktop was up. It now waits, 200 ms at a time,
until the case ends. `net-http` still passes.

**DNS:** `cargo test -p zoo-abi`: 13 passed. That includes the query
bytes, a reply through a CNAME, and the rejections (wrong id, NXDOMAIN,
server error, cut-off reply, no address).

**Updated:** `dock-menu` and `windows` expect the new window in slot 5
(five apps now).

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 84 cases `OK`.
`assistant-live` passes with `--accel whpx` at `ZOO_OS BOOT
version=0.68.0`. `cargo test -p xtask` (25), clippy on `zoo-user` and
`xtask`, and `cargo fmt --check`: clean.

## Network steps 1 and 2 results: virtio-net, IPv4, TCP (version 0.67.0)

Designs: docs/n1-design.md, docs/n2-design.md.

| Case | Network | Result |
| --- | --- | --- |
| `net-ping` | isolated (`restrict=on`) | `OK`: DHCP offer `10.0.2.15` from `10.0.2.2`; no router given, so the DHCP server is used; its MAC `52:55:0a:00:02:02`; three ping replies |
| `net-open` | unrestricted | `OK`: the lease names gateway `10.0.2.2` and DNS `10.0.2.3`; three ping replies |
| `net-http` | isolated + `guestfwd` to a harness server | `OK`: connected to `10.0.2.100:80`; ``92 bytes, status `HTTP/1.0 200 OK`, body `Hello from the host, ZooOS!` ``; the server answered 1 request |
| `snowgoose-no-net` (negative) | none | rejected as intended: ``got `... snowgoose-test code=2` `` |

**Negative controls**, each run once and then reverted:

| Broken on purpose | Result |
| --- | --- |
| The driver never returns a received frame | `FAIL net-ping` (`DHCP failed: no DHCP offer`, exit 4) |
| No server behind 10.0.2.100:80 | `FAIL net-http` (`cannot connect to 10.0.2.100:80: connection refused`, exit 6) |

**Found while testing:**
- **The isolated network sends no router or DNS server** (the first run
  said gateway `0.0.0.0`). The checks now use the DHCP server's address.
- **`tern-test` and `puffin-test` were already C programs.** The first
  full run failed `fast-boot`, `args-clock` and `timer-rate` because the
  new Rust programs replaced them, so the new ones were renamed.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 83 cases `OK`.
`assistant-live` passes with `--accel whpx` at `ZOO_OS BOOT
version=0.67.0`. `cargo test -p xtask` (25 tests), clippy on the kernel
(normal and `fast-boot`), `zoo-user` and `xtask`, and `cargo fmt --check`:
clean.

## Assistant step 2 results: the Assistant learns (version 0.66.0)

Design: docs/a2-design.md.

**New case `assistant-memory`** (TCG, no model needed): `OK`.
- `where is my pancake recipe` → no model; `search pancake` typed next →
  ``Assistant: learned `where is my pancake recipe` -> `search pancake` ``.
- `where is the pancake recipe` → ``recalled `search pancake` (100% alike)``:
  `1-recalled` shows `Assistant (remembered): search pancake` and the
  result.
- After a desktop restart, `pancake recipe where is it` →
  ``recalled `search pancake` (80% alike)``; `make a folder called
  Invoices` is not recalled and goes to the model (`2-after-restart`).

**Negative control:** recall switched off (`RECALL_AT` = 101) →
`FAIL assistant-memory` (timeout waiting for the recall).

**Updated case:** `assistant-model` (and `assistant-live`) now expect the
broker (pid 31) before the model service (pid 32): the Assistant asks its
memory first. The first full run caught this
(``got `ZOO_OS PROC start pid=31 name=otter-handler` while expecting
`... elephant-handler` ``); after the fix `assistant-model` and
`assistant` pass on TCG and `assistant-live` with `--accel whpx`
(`ZOO_OS BOOT version=0.66.0`).

**Full `cargo xtask verify --skip-model`** (TCG): 78 cases `OK`. The one
`FAIL` was `assistant-model` (above), which was fixed and rerun alone.

## Assistant step 1 results: a faster model engine (version 0.65.0)

Design: docs/a1-design.md (all timing tables are there).

| Check | Result |
| --- | --- |
| `cargo xtask infer-check` (NumPy reference, 5 prompts) | `PASS`: same tokens; top-5 logits within 0.00006 |
| `cargo xtask assist-check` (8 requests) | `PASS`: same tools, arguments and margins as before |
| `cargo test -p zoo-infer` | 10 passed |
| `inference`, `--accel whpx` | `OK`: tokens match the host engine exactly; prefill 437 → 164 ticks, generation 116 → 58 ticks |
| `assistant-live`, `--accel whpx` | `OK`: system prompt 922 → 340 ticks; requests 1155 → 437 and 369 → 151 ticks |

**The integer-activation variant** was measured (prefill 16.7 tok/s,
generation 12.3 tok/s on the host) and **rejected by `infer-check`**
(``top-5 logit max diff 0.21225``, limit 0.02), so it is not used.

The full TCG suite was not rerun for this change: it touches only
`zoo-infer` and the model service, whose cases are the two above. It runs
with the next step (A2).

## UI steps 3f and 3g results: Finder-style Files, the editor, icons (version 0.64.0)

Designs: docs/u3f-design.md, docs/u3g-design.md.

**New case `finder`** (TCG): `OK`.
- `1-sidebar`: `Favourites` (dim), `Home` selected, `Documents` in the
  sidebar; Documents' folder icon at (312, 127); the `Folder` column.
- `2-code`: `zoo.rs` focused; keyword `struct`, string `"Peacock"`,
  comment, type `u32`, macro `println!` and function `main` each found in
  their syntax colour, and `struct` not in the text colour. Ctrl+S logged
  `Notes: saved 364 bytes` (360, `end` and the Enter the test types).
- `3-folder`: a right-click on empty space, New Folder: `untitled folder`
  created on disk and selected.

**Updated cases:** folder names without `/` (`files`, `desktop`,
`appearance`, `redraw-content`, …); `README.txt` and `34 bytes` checked
apart; `notes-cursor` finds the cursor bar between `aX` and `bc`;
`apps-folder` and `dock-menu` find Notes by its icon's yellow band.

**Negative controls**, each run once and then reverted:

| Broken on purpose | Result |
| --- | --- |
| Notes colours nothing (every file plain) | ``FAIL finder: screen 2-code: keyword: `struct` not on screen`` |
| Remove from Dock keeps the pin (new icons) | ``FAIL dock-menu: screen 2-removed: Notes' yellow band is still in the dock (27 pixels on row 1013)`` |

**Bugs found while testing:**
- **The window server overflowed its stack at start-up.** Each window's
  lines grew to 240 bytes, so the line buffers moved to the heap.
- **A panic in `f32::clamp`.** It panics when rounding crosses its bounds,
  which happened on a 0.06-wide rounded rectangle in the Crash Test icon.
  `rr` now uses `max`/`min`.

**Full `cargo xtask verify --skip-model`** (TCG): 77 cases `OK`, with one
`FAIL`: `appearance` still looked for `Documents/` with the slash. The
check was fixed and `appearance` rerun alone: `OK`. `assistant-live`
passes with `--accel whpx` at `ZOO_OS BOOT version=0.64.0`.
`cargo test -p xtask` (25 tests), clippy on `zoo-user` and `xtask`, and
`cargo fmt --check`: clean.

## UI step 3 results: windows, dock menus, Applications (version 0.63.0)

Design: docs/u3-design.md.

**New cases** (TCG, in the full run): all `OK`.

| Case | Shows |
| --- | --- |
| `windows` | Ctrl+N: a second Files window 32 px down and right, focused; closing it leaves the first |
| `dock-menu` | Files' dock menu with `New Window` highlighted; New Window opens window 4; Remove from Dock leaves a three-icon dock without Notes |
| `dock-minis` | two minimised windows after the divider, each with its badge; a thumbnail click brings Files back |
| `apps-folder` | Remove Notes; Files → Applications → right-click Notes → Add to Dock (`dock saved (4 kept)`); remove again, restart the desktop: `dock loaded (3 kept)` and no Notes in the dock |

`animations` now restores the window through its thumbnail.

**Negative controls**, each run once and then reverted:

| Broken on purpose | Result |
| --- | --- |
| Ctrl+N does nothing | `FAIL windows` |
| Remove from Dock logs but keeps the pin | ``FAIL dock-menu: screen 2-removed: Notes' yellow is still in the dock`` |
| No divider or thumbnails drawn | ``FAIL dock-minis: screen 1-minis: no divider at x 1023`` |
| `.dock` read but ignored | `FAIL apps-folder` (timeout: it logged `dock loaded (4 kept)`, and the case waits for `3 kept`) |

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 77 cases `OK`.
`assistant-live` passes with `--accel whpx`, at `ZOO_OS BOOT
version=0.63.0`. `cargo test -p xtask` (25 tests), clippy on `zoo-user`
and `xtask`, and `cargo fmt --check`: clean.

## UI step 2f results: the ZooOS mark (version 0.62.0)

Design: docs/u2f-design.md.

**New case `logo`** (`--accel whpx`): `OK`. ``the soft paw: main pad filled
at (18, 16), gap between the upper toes at (18, 7)``. `gpu-hidpi` checks
the same points at 2x, (36, 32) and (36, 14): `OK`.

**Negative control:** the old disc mark → ``FAIL logo: ... toe gap (18, 7)
is Some([53, 55, 57]) (want [223, 233, 241])``.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 73 cases `OK`.
`assistant-live` passes with `--accel whpx`, at `ZOO_OS BOOT
version=0.62.0`. clippy on `zoo-user` and `xtask`, and `cargo fmt
--check`: clean.

## UI step 2e results: the dock hides itself (version 0.61.0)

Design: docs/u2e-design.md.

**New case `dock-autohide`** (`--accel whpx`): `OK`.
- `1-hidden`: wallpaper where the dock would be.
- `2-shown`: after `dock shown` at the bottom edge, running dots under
  the first three apps.
- `3-hidden-again`: after `dock hidden`, wallpaper again.

**Updated cases:** `dock`, `animations`, `notes-1`, `notes-cursor` and
`genie` all pass with the dock revealed at the bottom edge before each
icon click. `dock` gained `4-dock-reopened` (the running dot while the
dock is up), and `5-reopened` now checks that the dock hid.

**Negative control:** a dock that never hides → ``FAIL dock-autohide``
(no `dock hidden` within 60 s).

**0.61.1, the reveal zone:**
- **The bug:** the owner found the dock would not come back in a
  maximised QEMU window. Its scaling stops the pointer a few rows short of
  the last one, and the reveal zone was the last two rows.
- **The fix:** it is now six rows (`DOCK_EDGE`).
- **The test change:** `dock-autohide` reveals from 4 rows above the edge.
- **Control:** the old two-row zone → `FAIL dock-autohide` (timeout).
- **Checks run:** `dock-autohide`, `dock` and `animations` are `OK` under
  `--accel whpx`. The full suite was not rerun for this one-constant
  change.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 72 cases `OK`.
`assistant-live` passes with `--accel whpx`, at `ZOO_OS BOOT
version=0.61.0`. clippy on `zoo-user` and `xtask`, and `cargo fmt
--check`: clean.

## UI step 2d results, the genie (version 0.60.0)

Design: docs/u2d-design.md §6.

**New case `genie`** (`--accel whpx`): `OK`.
- A slow-motion minimise (Shift held): `frames 522 time 5209 ms`.
- Screenshot `1-genie`: the title bar still spans the top at (115, 66),
  and the bottom-left corner (127, 707) shows wallpaper.

**Negative control:** minimise drawn as the old shrink → ``FAIL genie:
the top edge moved: title bar [232, 232, 232] expected at both ends,
found Some([132, 179, 221])``.

**`animations`**, three runs: minimise 53 frames in 523 ms, restore 47–48
frames in 460–469 ms. Genie frames: median 6.7–7.8 M cycles.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 71 cases `OK`.
`assistant-live` passes with `--accel whpx`, at `ZOO_OS BOOT
version=0.60.0`. clippy on `zoo-user` and `xtask`, and `cargo fmt
--check`: clean.

## UI step 2d results: window animations (version 0.59.0)

Design and the measurement table: docs/u2d-design.md.

**New case `animations`** (`--accel whpx`), each animation's end line in
range:

| Animation | Frames | Time |
| --- | --- | --- |
| Close Crash Test | 19 | 180 ms |
| Minimise Files | 34 | 328 ms |
| Restore Files | 30 | 287 ms |
| Open Crash Test | 24 | 229 ms |

The final screenshot shows `Crash Test` focused at (1348, 524) and
`Files` back at (503, 84). Frames during minimise and restore, three
runs: median 0.95–1.41 M cycles, longest 17.5–23.3 M (a layer rebuild on
refocus, §3 of the design).

**Negative control:** zero-length animations → ``FAIL animations: `frames`
1 outside 3..=10000`` (close Crash Test).

**A visual check:** a mid-animation frame, from a temporary build with a
4 s minimise (the edit was restored and compared with `cmp`), shows Files
scaled down on its way to the dock.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 70 cases `OK`.
`assistant-live` passes with `--accel whpx`, at `ZOO_OS BOOT
version=0.59.0`. `cargo fmt --check`, plus clippy on `zoo-user` and
`xtask`: clean.

## UI step 2c results: the pointer moves with a dragged window (version 0.58.1)

Design: docs/u2c-design.md.

**New case `gpu-drag-cursor`** (`--accel whpx`): `OK`.
- `1-dragging`: ``mid-drag: the arrow is in the frame at (839, 136),
  with the window``.
- `2-released`: ``after the drag: no arrow in the frame at (839, 136)
  (the hardware cursor is back)``.

**Negative control:** the pointer never drawn in the frame during drags
→ ``FAIL gpu-drag-cursor``: no ``PeacockHandler: pointer drawn in the
frame`` line within 60 s.

**Found during the work:**
- The first version crashed the window server when a drag began: a stack
  overflow from two 16 KiB cursor images, `fault=page-fault access=write
  cause=not-present addr=0x00007fffffeffac8`. Both now live on the heap.
- The first version of the case ended with a click on Crash Test's close
  light, which the dragged Files window now covered. That step was
  removed.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 69 cases `OK`.
`assistant-live` passes with `--accel whpx`. The owner tried the change
in a session and approved it.

**Other checks:** `cargo fmt --check`, plus clippy on `zoo-user` and
`xtask`: clean.

## UI step 2b results: the monitor's resolution, 2x for 4K (version 0.58.0)

Design and measurements: docs/u2b-design.md. Also recorded here: 0.57.1
(pirchh/ZooOS#65), frame pacing and the new pointer, whose full verify
was `PASS` with 65 cases `OK`.

**New cases** (all with `--accel whpx`):

| Case | Result |
| --- | --- |
| `gpu-hidpi` | `OK`: `gpu host size 3840x2160`, `2x scale, 1920x1080 logical`. A tablet click on the close light closes Crash Test. The screenshot has 26 px text with the title at (1006, 168), twice its 1x (503, 84), and no 13 px title. |
| `gpu-1440` | `OK`: the owner's 2560×1440 at 1x. The `Files` title is at (676, 84), as the 2560×1440 layout puts it. |
| `gpu-hidpi-frames` | `OK`: see the measurements in docs/u2b-design.md §5 |
| `big-screen` | `OK`: a 2560×1600 screen is now used whole |

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 68 cases `OK`.
Then with `--accel whpx`: `assistant-live` 31.1 s, `wine-home-1` 62.7 s,
`wine-home-2` 11.1 s, all `OK`, with `ZOO_OS BOOT version=0.58.0`.

**Other checks:**
- `cargo test -p xtask --locked`: 25 passed, including the atlas test
  over all eight faces.
- clippy `-D warnings`: kernel (both modes), `zoo-abi`, `zoo-user` and
  `xtask`, all clean.
- `cargo fmt --all --check`: clean.

**Negative control:** the window server never chooses 2x → ``FAIL
gpu-hidpi`` (a timeout waiting for the 2x marker and the click on the 2x
layout). The first attempt at this control did not apply its edit
(rustfmt had wrapped the line) and so passed. It was redone, and failed
as it should.

**The host:** `xtask run` on the owner's PC, with no `host monitor` line,
showed that the primary monitor is not 4K. `EnumDisplaySettings` gives
`DISPLAY1 2560x1440 @ 59Hz`.

## UI step 2a results: a compositor in the window server (version 0.57.0)

Design and the full measurement table: docs/u2-compositor-design.md §3.

**New measurements** (`frames` and `gpu-frames`, WHPX):
- the frame report now gives the cycles spent showing each frame;
- the cases now also measure a window drag (30 moves of Files' title
  bar) and content updates (Down and Up five times each in Files).

**Before** (first runs on this branch, without the compositor), drag
frames were full repaints: 7,674,908, 7,851,347 and 7,974,669 cycles on
VGA; 9,836,683, 9,914,764 and 9,983,554 on the GPU.

**After:**

| Frame | VGA | GPU |
| --- | --- | --- |
| Drag | 2,297,288–2,313,467 for 630,720 px | 2,464,719–2,535,777 |
| One-line selection move | 270,184–289,975 for 35,376 px | 384,130–506,243 |
| Pointer | 14,871–16,313 | none |
| Full repaint | 5,594,222–5,664,185 | 5,064,211–5,283,560 |

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 64 cases `OK`,
including the new `redraw-content`. Then with `--accel whpx`:
`assistant-live` 31.2 s, `wine-home-1` 63.2 s, `wine-home-2` 11.1 s,
`gpu-tablet`, all `OK`, with `ZOO_OS BOOT version=0.57.0`.

**Other checks:**
- `cargo test -p xtask --locked`: 25 passed.
- clippy `-D warnings`: kernel, `zoo-abi`, `zoo-user` and `xtask`, all
  clean.
- `cargo fmt --all --check`: clean.

**Negative controls** (temporary edits, restored and compared with `cmp`):

| Change | Result |
| --- | --- |
| A look change leaves the layer cached | ``FAIL redraw: screen 1-partial: `Crash Test` not on screen`` |
| A drag damages only the new position | `FAIL redraw: screen 2-full: 34828 of 2023680 pixels differ` |
| The in-place content update writes nothing | ``FAIL redraw-content: screen 1-content: `Photos/` not on screen`` |

**Found during the work:**
- The first versions of the first and second controls passed. The old
  `redraw` case ended with the same focus it started with, and dragged in
  one move followed by a repaint large enough to hide trails. It now
  drags in four moves and ends with Crash Test focused. A content check
  placed in it was hidden by a later focus change, so it became its own
  case, `redraw-content`.
- In-place content updates first never ran: applications resend their
  title with every update, and any title message counted as a change.

## GPU step 1 results: virtio-gpu display and a hardware cursor (version 0.56.0)

Design: docs/gpu1-design.md.

**Boot log** (`virtio-vga`): `ZOO_OS INFO gpu virtio 1920x1080 (shown when
the display is taken)`, then on `desktop` `ZOO_OS INFO gpu scanout
1920x1080 hardware-cursor` and `PeacockHandler: hardware cursor`. When the
display is released: `ZOO_OS INFO gpu released (vga)`.

**New cases** (all on `virtio-vga`):

| Case | What it shows |
| --- | --- |
| `gpu-desktop` | The `desktop` case's five screenshots pass on the GPU. The window server's crash releases the GPU and retakes it (log above, twice), and the console comes back in VGA mode (`kernel console back: Desktop session ended. row 6 ...`). |
| `gpu-tablet` | Tablet click on the 12 px close light closes Crash Test. The 40-move sweep draws 3 frames, the menu-bar clock (49,920 px = 1920×26); input wait mean 380,351 cycles. The screenshot shows the desktop (`Files content Home at (131, 117); Crash Test closed`). |
| `gpu-frames` | F9 full repaints with flushes, WHPX, three runs: median 10,211,885, 10,045,071 and 9,904,380 cycles for 2,073,600 px. Standard VGA today: 8,537,538–11,018,483. |

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 63 cases `OK`.
Then with `--accel whpx`: `assistant-live` 31.4 s, `wine-home-1` 63.5 s,
`wine-home-2` 11.1 s, `gpu-tablet`, `gpu-desktop`, all `OK`, with `ZOO_OS
BOOT version=0.56.0`.

**Other checks:**
- `cargo test -p xtask --locked`: 25 passed.
- clippy `-D warnings`: kernel (normal and `fast-boot`), `zoo-abi`,
  `zoo-user` and `xtask`, all clean.
- `cargo fmt --all --check`: clean.

**Negative controls** (temporary edits, restored and compared with `cmp`):

| Change | Result |
| --- | --- |
| The window server never calls `display_flush` | ``FAIL gpu-tablet: screen 1-gpu-desktop: `Home` not on screen`` |
| The pointer drawn in software despite the hardware cursor | ``FAIL gpu-tablet: `frames` 43 outside 0..=5`` (each pointer frame then cost a median 128,692 cycles, flush included) |
| No device reset when the display is released | ``FAIL gpu-desktop: screen 4-server-restart: `Files` not on screen`` |

**Found during the work:** the first sweep bound allowed no frames at
all, but the menu-bar clock ticks during the sweep. The bound is now 5,
against 40–43 frames with a software pointer.

**0.56.1, the windowed size:**
- In the owner's QEMU window, the GPU's preferred mode was 640×480:
  `ZOO_OS INFO gpu virtio 640x480`. QEMU's GTK display reports its window
  size, and the headless cases report the configured 1920×1080.
- The driver now keeps the size between 1920×1080 and 1920×1200.
- `cargo xtask run --accel whpx --timeout-secs 40` (windowed) then logged
  `gpu virtio 1920x1080`, `gpu scanout 1920x1080 hardware-cursor` and
  `PeacockHandler: hardware cursor`.
- `gpu-tablet`, `gpu-desktop`, `gpu-frames` and `usb` are `OK` with
  `--accel whpx`, at `ZOO_OS BOOT version=0.56.1`. Only the virtio path
  changed, so the full suite was not rerun.

## Fix: `cargo xtask run` ignored `--accel` (version 0.55.0)

- **The bug:** `run` parsed `--accel` but never called `qemu::set_accel`,
  so every interactive session ran under TCG, including the owner's U1–U1c
  sessions started with `--accel whpx`. `build/runs/run-fast-boot/
  qemu-command.txt` showed `"-accel" "tcg"`. The verify cases were
  unaffected: `verify` applies the flag.
- **After the fix:** `cargo xtask run --accel whpx --timeout-secs 20`
  prints `[run] booting fast-boot image in a QEMU window (accelerator
  whpx)`. Its command line has `"-accel" "whpx"`, and the guest's local
  APIC calibrates at `per-tick=125062`, against 626206 under TCG.
- **New:** `run --model` boots the normal image with the assistant's model
  added (built as `assistant-live` builds it). Without it, the everyday
  fast-boot image has no model, and the Assistant answers only its typed
  commands.

## UI step 1c results: USB input by interrupt, and a tablet (version 0.55.0)

Design: docs/u1c-design.md.

**Interrupts:** under QEMU `qemu-xhci` offers MSI-X only (capabilities at
0x90, MSI-X, and 0xA0, PCI Express). The boot log shows `ZOO_OS INFO usb
interrupts=msix`.

**USB mouse wait** (`--accel whpx --case usb-desktop`, 40-event sweep, TSC
cycles from the controller's interrupt to `MOUSE_READ`):

| Build | Mean | Longest |
| --- | --- | --- |
| Interrupt only stamps; reports taken at the tick (the old timing) | 17,009,645 and 19,916,866 | 31,775,496 and 146,450,574 |
| Reports taken in the interrupt (three runs) | 433,305–520,623 | 1,176,026–2,180,180 |

**Tablet** (`--accel whpx --case tablet`, three runs):
- the report descriptor was read as `ZOO_OS INFO usb pointer absolute
  x=0..32767 y=0..32767 buttons=3`;
- an absolute move onto Crash Test's 12 px close light and a click closed
  it;
- sweep wait: mean 386,161–485,827, longest 940,619–2,876,353 cycles.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 60 cases `OK`
(the new `tablet` included). Then with `--accel whpx` at version 0.55.0:

| Case | Result |
| --- | --- |
| `assistant-live` | `OK`, 31.2 s |
| `wine-home-1` | `OK`, 62.3 s, back near U1's 63.9 s, so U1b's 72–76 s was the machine |
| `wine-home-2` | `OK`, 11.0 s |
| `tablet`, `usb`, `usb-hub` | `OK`, boot log `ZOO_OS BOOT version=0.55.0` |

**Other checks:**
- `cargo test -p xtask --locked`: 25 passed.
- clippy `-D warnings`: kernel (normal and `fast-boot`), `zoo-abi`,
  `zoo-user` and `xtask`, all clean.
- `cargo fmt --all --check`: clean.

**Negative controls** (temporary edits, restored and compared with `cmp`):

| Change | Result |
| --- | --- |
| The xHCI interrupt only stamps; reports are left to the tick | ``FAIL usb-desktop: `tick: mean` 624 outside 0..=250`` (longest 4591), then 533 (longest 997) |
| The window server treats absolute positions as motion | `FAIL tablet`: ``no `ZOO_OS PROC exit pid=29 name=crash-test code=0` within 60s`` (the click missed the light) |

**Found during the work:**
- The first MSI-only version fell back to polling (`usb interrupts=polled`)
  because QEMU's controller has no MSI capability.
- The first `tablet` run passed its steps, but its expectation did not
  list Crash Test's exit after the close.

## UI step 1b results: input without waiting for the tick (version 0.54.1)

Design: docs/u1b-design.md. Prompted by the owner finding U1 jittery.

**Input wait** (`--accel whpx --case frames`: partial-redraw sweep, 40
events, three runs each; TSC cycles from a mouse event's arrival to
`MOUSE_READ`):

| Build | Mean | Longest |
| --- | --- | --- |
| Before (window server polls each tick) | 14,399,606–16,097,558 | 30,875,290–32,043,520 |
| Input wakes the server; idle kernel still halts with a program ready | 60,848–839,125 | 145,766–31,946,041 |
| As merged | 52,523–89,478 | 113,111–533,743 |

A tick is about 31 M cycles here. A pointer frame still costs
57,827–65,989 cycles, so drawing is unchanged.

**Without an accelerator** (`--case frames`): `OK`, `input wait 40 events
mean 181501 max 472443 cycles; per mille of a tick: mean 5 longest 14`.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 59 cases `OK`.
`assistant-live`, `wine-home-1` and `wine-home-2` were then run with
`--accel whpx`, all `OK`.

**Wine timing:** `wine-home-1` took 72.6–75.9 s over five runs, against
63.9 s when U1 was measured. The same day, the kernel files from `main`
took 71.3 and 71.7 s, so most of that gap is the machine, not the change.
With the idle yield disabled it took 75.0 and 75.2 s. No cause was found
for the remaining few percent, and none is claimed.

**Other checks:**
- `cargo test -p xtask --locked`: 25 passed.
- clippy `-D warnings`: kernel (normal and `fast-boot`), `zoo-user` (its
  own target), `zoo-abi` and `xtask`, all clean.
- `cargo fmt --all --check`: clean.

**Negative controls** (temporary edits, restored and compared with `cmp`):

| Change | Result |
| --- | --- |
| Window server back to `sleep(1)` | ``FAIL frames: `tick: mean` 459 outside 0..=250`` (longest 954) |
| Idle kernel task halts even with a program ready | ``FAIL frames: `longest` 1050 outside 0..=500``, then `1009` on a second run |

**Found during the work:** the first check read the reset report printed
just before the sweep (`input wait 0 events`). It now waits for the
sweep's own frame report (at least 20 frames) first.

## UI step 1 results: a macOS-style desktop (version 0.54.0)

Design: docs/u1-design.md. Inter 4.1 from tools/fonts/README.md
(checksums there).

**Screen:** `ZOO_OS INFO console 1920x1080` and `PeacockHandler: display
1920x1080 acquired`. The big-screen case still gets 2560×1600 and uses
1920×1200 of it.

**Checks moved to the new look:**
- `screen::find_ui` renders expected text from the same atlas with the
  same layout code (`zoo_abi::font::Face::draw`) and searches the
  screenshot for an exact match.
- Positions come from `zoo_abi::ui` through a pointer tracker.
- Cases updated: `desktop`, `mouse`, `redraw`, `dock`, `notes-1`,
  `notes-2`, `notes-cursor`, `resize`, `zoom`, `files-browse`,
  `menu-clock`, `assistant`, `assistant-model`, `big-screen`,
  `usb-desktop`.
- Crash notices are now checked in the banner, and the dock check uses
  hover names and running dots.

**New case `appearance`:** F8 turns the desktop dark (`Files` title,
content, selection, menu bar and wallpaper in the dark palette, nothing
left in the light one). Then a click on the menu bar's switch turns it
light again.

**`redraw` strengthened:** the pointer now also ends a crawl across a
text line with its area over the top two rows of the capitals. Without
that, the first negative control below passed.

**Speed** (`--accel whpx --case frames`, three runs each; numbers in
docs/u1-design.md §4):
- full repaint: 9.4–9.9 M cycles for 2.07 M px;
- pointer move: 59,149–62,200 cycles, against 31,158–31,718 in D2 at
  1280×800;
- a first version that drew whole shapes outside the redrawn area took
  751,112–962,589.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 59 cases `OK`;
`assistant-live`, `wine-home-1`, `wine-home-2` `SKIP`. Then with `--accel
whpx`: `assistant-live` 31.6 s, `wine-home-1` 63.9 s, `wine-home-2` 11.5 s,
all pass.

**Other checks:**
- `cargo test -p xtask --locked`: 25 passed, including the atlas test
  (`font::tests::atlas_parses_and_lays_out`).
- `cargo clippy -D warnings`: kernel (both modes), `xtask`, `zoo-user`,
  `zoo-abi`, 0 warnings.
- `cargo fmt --all --check`: clean.

**Negative controls** (temporary edits, restored and compared with `cmp`):

| Change | Result |
| --- | --- |
| Text skipped unless the redrawn area reaches 8 px above its baseline | First `OK` (the check could not see it); after strengthening `redraw`: `FAIL`, `2 of 2023680 pixels differ from the partial redraws, first at (1070, 547)` |
| Switching appearance without a full repaint | `FAIL appearance`: ``screen 1-dark: `Files` not on screen`` |

**Found during the work:**
- The checker first kept one coverage value per pixel, but neighbouring
  glyphs overlap: in `Assistant` one pixel is covered by both `n` and
  `t`, and the screen blends both. The checker now blends in drawing
  order.
- A panic (`slice index starts at 1967008 but ends at 1966943`): the
  translucency helper built a backwards slice for a rectangle outside the
  redrawn area.
- The atlas parser read the face header at the wrong offsets (10/14/18 for
  8/12/16); the new atlas test exercises it.
- Inter 4 has no legacy `kern` table, so the atlas carries no kerning.

## Linux step 10d results: Wine's prefix in the home folder (version 0.53.0)

Design: docs/linux10d-design.md.

**New cases:**
- `wine-home-1` (`--accel whpx`, fresh shared disk), 66.3 s: `linux
  /usr/bin/wine cmd /c echo %OS%` with the default prefix
  `/home/user/.wine`. Wine creates the prefix, `cmd.exe` prints
  `Windows_NT`, `wine` exits 0 and wineserver exits.
- `wine-home-2` (same disk), 11.7 s: BadgerHandler loads the saved prefix
  and `cmd.exe` prints `Windows_NT` again. Other runs: 68.6 s / 11.5 s and
  76.8 s / 12.1 s. Without an accelerator both report `SKIP`: the first
  passed every step but did not finish creating the prefix within 480 s
  (167 MB at about 350 KB/s). The second alone passed in 20.5-22.4 s
  under TCG.
- `linux-petrel-1`, `petrel-test write`:
  - names: a 34-byte name with `:` and brackets, a 200-byte name, a
    256-byte name refused with `ENAMETOOLONG`;
  - links: relative, to a folder, out to `/tmp`; `EEXIST`; renaming the
    folder that holds a link; unlinking a link;
  - `*at` calls relative to an open home folder;
  - `mmap` of a home file, whole and at an offset; a private write that
    stays private; a writable shared mapping refused (`ENODEV`);
  - `utimensat`.
- `linux-petrel-2`: the same disk after a reboot. The names and links are
  there, and so is a file that was written and never closed before exit.

**Failures met during the work:**

| Symptom | Cause | Fix |
| --- | --- | --- |
| `errno 38` from Wine's installer for every file | `utimensat`/`futimesat` missing | accepted for existing files |
| `copy error 1812` loading resource strings | no `mmap` of home files | private-copy `mmap` |
| 15,616 write requests for one registry save | musl's 1 KiB stdio buffer, one request each | gathered writes |
| `petrel-test` exit 13 (`EFAULT`) for a 275-byte path | Linux paths capped at 256 bytes | 4,096, `ENAMETOOLONG` beyond |
| `readlink` of a missing home path gave `EINVAL` | existence not checked | `Stat` first |
| 26.6 GB of the host disk used by test disks | NTFS reserves the whole length of an extended file (53 × 1 GiB) | sparse images (`fsutil sparse setflag`); per-case disks deleted after the case |

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 58 cases `OK`;
`assistant-live`, `wine-home-1`, `wine-home-2` `SKIP`. Then with `--accel
whpx`: `assistant-live` 32.4 s, `wine-home-1` 66.3 s, `wine-home-2`
11.7 s, all pass.

**Other checks:**
- `cargo test -p xtask --locked`: 25 passed.
- `cargo clippy -D warnings`: kernel (both modes), `xtask`, `zoo-user`,
  `zoo-abi`, 0 warnings.
- `cargo fmt --all --check`: clean.

**Negative controls** (temporary edits, restored and compared with `cmp`):

| Change | Result |
| --- | --- |
| Path resolution ignores home-folder links | `FAIL linux-petrel-1`: exit 25, `write through a link to /tmp (errno 20)` |
| BadgerHandler drops links when loading | `FAIL linux-petrel-2`: exit 42, `readlink (errno 2)` |
| Exit does not send gathered writes | `FAIL linux-petrel-2`: exit 61, `write before exit kept` |

## Linux step 10c results: ZooFS 2 (version 0.52.0)

Design: docs/linux10c-design.md.

**Why, measured.** After `wine cmd`, a temporary tree walker over
`/tmp/wine` counted 880 files, 124 folders, 4 links, 167,234,870 bytes;
the largest is `shell32.dll` at 9,193,459 bytes. `wine-cmd` takes
44.1 s with `--accel whpx`, against 176-260 s under TCG.

**New cases:**
- `linux-murre-1`: `murre-test write` puts 40 MiB, then 300 small files,
  a truncate, a rename and an unlink in `/home/user/Murre`. Saves are
  forced part-way (generations 1-5, 2,048 blocks each); generation 6
  holds the rest: 10,546 blocks in use, 10,807 bytes of metadata.
- `linux-murre-2`: the same disk, rebooted. `murre-test check` reads
  every byte back, including the overwritten region that starts off a
  block boundary.
- `persist-4`: generation 2 is saved to slot B, then a save is cut
  short.
- `persist-5`: generation 2 is damaged; generation 1 still reads `first
  version`, although the cut-short save ran after it.

**Changed:** `arcticfox-test` checks the 4 GiB file limit (was 16 MiB);
`corrupt_newest_snapshot` damages ZooFS 2 metadata.

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 56 cases `OK`,
`assistant-live` `SKIP`. Then `--accel whpx --case assistant-live`: pass
in 32.1 s.

**Speed**: `linux-fulmar`'s 1 MiB write and read, in kcycles under TCG,
five runs each. `main` was built in a temporary worktree.

| Build | Write | Read |
| --- | --- | --- |
| `main` (in-memory store) | 15,302 / 15,573 / 16,743 / 19,389 / 28,825 (median 16,743) | 6,867 / 7,100 / 7,187 / 9,986 / 12,172 (median 7,187) |
| First ZooFS 2 (a sorted map and one allocation per block) | 19,977 / 20,252 / 22,008 / 22,489 / 36,226 (median 22,008) | 6,816 / 7,183 / 7,427 / 7,965 / 11,180 (median 7,427) |
| ZooFS 2 as merged (chunked slab) | 15,927 / 16,001 / 16,160 / 16,173 / 16,675 (median 16,160) | 7,395 / 7,513 / 7,554 / 7,751 / 7,967 (median 7,554) |

The merged version is level with `main`, within the noise of these runs.
`murre-test` wrote 40 MiB in 5.7-6.8 billion cycles, including the five
forced saves, and read it back from disk in 2.0-2.7 billion.

**Negative controls** (temporary edits, restored and compared with `cmp`):

| Change | Result |
| --- | --- |
| The previous generation's blocks not protected | `FAIL persist-5`: `cat` printed `fifth version` (from the cut-short save) instead of `first version` |
| Metadata CRC not checked | `FAIL persist-3`: the damaged generation 2 was loaded; no `cat` step within 90 s |
| Data blocks not written (metadata only) | `FAIL linux-murre-2`: exit 33, `byte 1 is 0, not 187` |

## Linux step 10b results: `/tmp` in RAM; Wine runs `cmd.exe` (version 0.51.0)

Design and findings: docs/linux10b-design.md.

**New case `wine-cmd`** (`wine` image): `linux WINEPREFIX=/tmp/wine
/usr/bin/wine cmd /c echo %OS%`.
- The case waits for cmd.exe's own output line, `[44:wine-preloader]
  Windows_NT`, then for `ZOO_OS PROC exit pid=10 name=wine-preloader
  code=0`. The shell's echo of the command contains `%OS%`, not
  `Windows_NT`, so it cannot match.
- Wine's processes (wineboot, services, rundll32, explorer, cmd) come
  and go in no fixed order, so the case ends with `Step::Rest`: any
  further markers except a kernel PANIC or EXCEPTION. A unit test covers
  `Rest`.
- Run time under TCG was 176.2 s, 188.5 s and 259.5 s in three runs.
  Almost all of it is creating the prefix, which lives in RAM. The
  timeout is 480 s.

**New cases:**
- `linux-auklet`:
  - `/tmp` folders, files and symbolic links (relative, to a folder, a
    loop giving `ELOOP`);
  - `openat`/`mkdirat`/`fstatat`/`renameat`/`unlinkat` relative to an
    open folder;
  - `DT_LNK`;
  - a file unlinked while open;
  - `MAP_SHARED` across `fork`; `pread`/`pwrite` against the mapping;
  - `MAP_PRIVATE` copy-on-write: unwritten pages follow the file,
    written pages do not; read-only, then `mprotect` to writable;
  - `memfd_create` with two mappings, one unmapped and the descriptor
    closed;
  - environment from `linux AUKLET=wing TERM=vt100 auklet-test`.
- `linux-loon`: a program spins without system calls; the shell still
  runs `ps` and lists it as ready.

**Changed expectations:**
- `linux-skua`: `RLIMIT_NOFILE` is 1024 and the home listing no longer
  has `.tmp`.
- `fault-heap-guard`: the guard page follows the 8 MiB heap
  (`0xffffc00000801000`).

**Full `cargo xtask verify --skip-model`** (TCG): `PASS`, 52 cases `OK`,
`assistant-live` `SKIP`. Then `--accel whpx --case assistant-live`: pass
in 33.0 s.

**Negative controls** (temporary edits, restored and compared with `cmp`):

| Change | Result |
| --- | --- |
| `kernel_has_input` does nothing | `FAIL linux-loon`: no `ZOO_OS SHELL line=ps` within 30 s |
| `MAP_PRIVATE` copies at `mmap` time | `FAIL linux-auklet`: exit 67 (`unwritten private sees the file`) |
| Initrd folders report inode 0 | `FAIL wine-cmd`: `could not load kernel32.dll, status c0000135`; timeout at 360 s |

**Failures met during the work:**

| Symptom | Cause | Fix |
| --- | --- | --- |
| `failed to remap the process USD` (L10a) | no shared mappings | tmpfs + `MAP_SHARED` |
| A Wine thread stuck at one address; the shell stopped answering | `mov %gs:0x30` faulted (no GS base); the faulting loop starved the idle kernel task | `ARCH_SET_GS`; input marks the kernel task busy |
| `could not load kernel32.dll`, Wine looking in `Z:\x86_64-windows` | every initrd folder had inode 0 | inodes from full paths |
| `ws2_32.so`: `Error loading shared library ntdll.so` | musl's search path lacks Wine's Unix library folder | `/etc/ld-musl-x86_64.path` in the `wine` image |
| `shell32.dll` status `c000011f` | 64 descriptors per process | 1,024 |
| Dozens of `explorer.exe /desktop`, then `fork` `EAGAIN` | private mappings were copies, so the session's shared objects never appeared (`Session object id doesn't match`); 32 task slots | copy-on-write; 512 tasks |
| Wine printed nothing at all | console and `/dev/null` both reported device number 0, which Wine reads as stderr being `/dev/null` | Linux device numbers |

## Linux step 10a results: Wine builds, starts and reaches its server (version 0.50.0)

Design and findings: docs/linux10-design.md.

**The build** (WSL Alpine 3.24.3, `tools/wine/build-wine.sh`, GCC 15.2.0
and MinGW GCC 15.2.0): Wine 11.0, 64-bit, lean. The first attempt failed
for lack of `linux-headers`; with them it built. `tools/wine/export.sh`
packed 958 files, 162 MB, into `build/wine/`. The `wine` image's initrd
is 164 MB.

**New case `wine-version`:** `linux /usr/bin/wine --version` prints
`[10:wine] wine-11.0` and exits 0.

**New checks in existing cases:**
- `linux-skua`: argument/environment layout, `getrlimit(RLIMIT_NOFILE)`
  = 64, and `umask`.
- `linux-fulmar`: `pwrite`/`pread` at an offset without moving the
  position, `fchdir`, `statfs`.
- `linux-ermine`: `lstat` of a bound socket is `S_ISSOCK`.

`linux-skua`'s listing now includes `.tmp`, as Linux `readdir` shows dot
names.

**Full `cargo xtask verify --skip-model`** (TCG): exit 0, 49 cases `OK`,
`assistant-live` `SKIP`. Then `--accel whpx --case assistant-live`: pass in
34.5 s.

**Negative controls** (temporary edits, restored and compared):

| Change | Result |
| --- | --- |
| Environment strings placed before the arguments | `FAIL linux-skua`: exit 6 (`environment layout`) |
| Bound socket names invisible to `stat` | `FAIL linux-ermine`: exit 19 (`lstat of a bound socket (errno 2)`) |
| The `libc.musl-x86_64.so.1` alias as a copy instead of a hard link | `OK`: musl never opens the file; the alias and the tar hard-link support were removed, and `wine-version` passes without them |

**Failures met during the work:**
- **The boot check `list` saw `.tmp`:** `Zookeeper code=27`. Fixed by
  hiding dot names in the Mac-style listings.
- **`wine` and L8's `libc.so`:** `Error relocating /usr/bin/wine: memcpy:
  symbol not found`, exit 127. Fixed by not linking Zig's compiler
  runtime (§4 of the design).
- **`wine-preloader` with the old string layout:** a write fault at
  `0x7ffefff0ffba`.
- **`umask` on 90 (which is `chmod`):** `skua-test` `umask` failed with
  `errno 38` until it was moved to 95.

Exploration not in the suite: `wine cmd /c echo %OS%` reaches Wine's
server and stops at `virtual_map_user_shared_data` (shared memory: L10b).

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`,
`zoo-user` for its target): 0 warnings. `cargo test -p xtask -p zoo-abi`:
24 and 10 passed. Not run: real hardware.

## Linux step 9 results: the address-space layout Wine needs (version 0.49.0)

Design and details: docs/linux9-design.md. New case `linux-redpoll`:
`redpoll-test` (freestanding, no C library, linked at `0x7d7d00000000`
like Wine's preloader) checks:
- kernel-chosen mappings are high;
- the low 2 GiB can be reserved, and `MAP_FIXED` at `0x400000` works;
- `MAP_FIXED_NOREPLACE` fails with `EEXIST` over a mapping and over the
  program itself;
- `munmap` carves one page that can be mapped again, with the neighbours
  intact;
- 4 MiB of live stack frames; `fork` keeps the layout;
- the reservation is free again after release.

All earlier Linux cases pass unchanged.

The downloads for the coming Wine build are recorded with their checks:
- Wine 11.0: a good signature from Alexandre Julliard's key, and a
  SHA-512 equal to Alpine 3.24's recipe (tools/wine/README.md).
- Alpine 3.24.3 minirootfs: a SHA-256 equal to Alpine's release index,
  and a good signature from Alpine's release key (tools/alpine/README.md).

**Full `cargo xtask verify --skip-model`** (TCG): exit 0, 48 cases `OK`,
`assistant-live` `SKIP`. Then `--accel whpx --case assistant-live`: pass in
35.7 s.

**Negative controls** (temporary edits to `kernel/src/process.rs`,
restored and compared):

| Change | Result |
| --- | --- |
| Kernel-chosen mappings start low | `FAIL`: exit 10 |
| `munmap` leaves the reservation in place | `FAIL`: exit 41 |
| The program is not a reservation | `FAIL`: `PROC crash pid=26 … access=execute cause=protection addr=0x00007d7d0000165d` |
| The stack is not extended as used | `FAIL`: `PROC crash pid=26 … access=write cause=not-present addr=0x00007fffffeffe80` |

The stack control first passed: the recursion had been turned into a
loop. The test was fixed so that every frame stays live, and the control
was run again.

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask -p zoo-abi --locked`: 24 and 10 passed.
Not run: real hardware.

## Linux step 8 results: dynamically linked programs (version 0.48.0)

Design and details: docs/linux8-design.md; source provenance:
tools/musl/README.md.

- **musl 1.2.5 source:** 1,080,786 bytes, checked three ways: SHA-256
  `a9a118bb…c75e4`; SHA-512 equal to Alpine `aports` 3.21-stable; a good
  signature from key `8364 8929 0BB6 B70F 99FF DA05 56BC DB59 3020 450F`.
- **`libc.so`:** built by `cargo xtask build` with Zig 0.16.0 (clang
  21.1.0, LLD), 885,856 bytes stripped.

New case `linux-dovekie`: `dovekie-test` (dynamic, needs
`libdovekie.so`) checks the library's functions, global, constructor and
per-thread TLS; `snprintf` called from the library; `dlopen`/`dlsym` of a
plugin calling back; `dlerror` for a missing library. It then starts 10
static and 10 dynamic programs. No unsupported system calls appear in its
log.

Found on the way:
- Libraries built without a soname were recorded by their Windows build
  path, so `-Wl,-soname` was added.
- Zig's `cc` driver drops `--dynamic-list` and `--exclude-libs`, so the
  link uses `zig ld.lld` directly.
- musl's complex multiplication needs a compiler runtime, so Zig's
  compiler-rt is built from the pinned toolchain.
- `membarrier` (324) was unsupported in the first run and was added.

**Measured** (`--skip-model --accel whpx --case linux-dovekie`, start +
wait averages of 10): static 2134 / 2276 / 2323 kcycles, dynamic 3307 /
2627 / 2824 kcycles.

**Full `cargo xtask verify --skip-model`** (TCG): exit 0, 47 cases `OK`,
`assistant-live` `SKIP`. Then `--accel whpx --case assistant-live`: pass in
38.2 s.

**Negative controls** (temporary edits to `kernel/src/process.rs`,
restored and compared):

| Change | Result |
| --- | --- |
| `AT_BASE` not given to the loader | `FAIL`: `PROC crash pid=26 … cause=not-present addr=0x2f0e9f08` |
| Every file reports inode 0 | `FAIL`: exit 41 (`plugin symbols`) |
| `MAP_FIXED` address ignored | `FAIL`: `PROC crash pid=26 … access=write cause=protection` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask -p zoo-abi --locked`: 24 and 10 passed
(new: the alltypes generator, `ET_DYN` placement, `PT_INTERP` checks).
Not run: real hardware.

## Linux step 7 results: Unix sockets and event waits (version 0.47.0)

Design and details: docs/linux7-design.md. New case `linux-ermine`:
`ermine-test` (static musl C) covers:
- `socketpair`, and `SCM_RIGHTS` across it;
- bind (with `EADDRINUSE`), listen, accept4, a forked client passing a
  pipe, `SO_PEERCRED`, end of data, `ECONNREFUSED` after close;
- `poll` (timeout, `POLLIN`, `POLLHUP`), `select`;
- `epoll` (data, `EEXIST`, `EPOLLONESHOT`, `MOD`, a cross-thread
  wake-up);
- `eventfd` (sum, semaphore, `EAGAIN`), `O_NONBLOCK` and `F_SETFL`;
- a timed ping-pong.

The program was first named `muskox-test`. That name already belongs to a
native boot-check program, so it was renamed before commit.

**Measured** (`--skip-model --accel whpx --case linux-ermine`): socket
round trip between two processes 8933 / 8956 / 9037 cycles (averages of
1000).

**Full `cargo xtask verify --skip-model`** (TCG): exit 0, 46 cases `OK`,
`assistant-live` `SKIP`. Then `--accel whpx --case assistant-live`: pass in
32.9 s.

**Negative controls** (temporary edits to `kernel/src/process.rs`,
restored and compared):

| Change | Result |
| --- | --- |
| Descriptors sent with `SCM_RIGHTS` are dropped | `FAIL`: exit 15 (`recvmsg SCM_RIGHTS`) |
| Poll-style timeouts end at once | `FAIL`: exit 23 (`poll on listener`) |
| `EPOLLONESHOT` not honoured | `FAIL`: exit 87 |
| Non-blocking descriptors block anyway | `FAIL` (timeout): no `start pid=28` within 60 s |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: 23 passed. Not run: real
hardware.

## Linux step 6 results: signal handlers, threads and lazy memory (version 0.46.0)

Design and details: docs/linux6-design.md. New case `linux-kittiwake`:
`kittiwake-test` (static musl C) covers:
- handlers with `siginfo`, blocked/pending signals, `SIGSEGV` with
  `siglongjmp`, a `SIGILL` handler editing `REG_RIP`, `sigaltstack`;
- `EINTR` / `SA_RESTART`, `pause`;
- pthreads with a mutex and a condition variable, a raw `clone` with
  `PARENT_SETTID` / `CHILD_CLEARTID`;
- a threaded child killed by `SIGTERM`, `ud2` ending a child with
  `SIGILL`;
- 64 MiB of lazy memory written by `read`, `clock_gettime` and `uname`.

**Measured** (`--skip-model --accel whpx --case linux-kittiwake`, three
runs, averages):
- `raise` + handler: 1725 / 1949 / 1827 cycles.
- `pthread_create` + `join`: 20 / 21 / 20 thousand cycles. Before lazy
  memory: 2570–2767 thousand. A temporary probe found 2,049-page
  `mprotect` commits (8 MiB thread stacks) costing about 2.3 Mcycles each.

**Full `cargo xtask verify --skip-model`** (TCG): exit 0, 45 cases `OK`,
`assistant-live` `SKIP`. Then `--accel whpx --case assistant-live`: pass in
37.2 s.

**Negative controls** (temporary edits to `kernel/src/process.rs`,
restored and compared):

| Change | Result |
| --- | --- |
| `rt_sigreturn` does not restore the mask | `FAIL`: exit 12 (`FAILED kill`) |
| `SA_RESTART` ignored | `FAIL`: exit 62 (`FAILED SA_RESTART (errno 4)`) |
| A thread's exit does not clear its clear-tid word | `FAIL` (timeout): no `start pid=28` within 60 s |
| No commit-and-retry for lazy memory | `FAIL`: exit 124 (`clock_gettime into an untouched page (errno 14)`) |

Removing only the clear-tid futex wake still passed. The kernel's general
wake-up on a thread's exit hides it; see the design document.

Two other controls first passed because the test did not reach what they
broke, so the test was strengthened:
- The clear-tid wake: musl's `pthread_join` does not rely on it, so the
  raw `clone` check was added.
- Commit-and-retry: pipe reads commit by themselves, so the
  `clock_gettime` and `uname` checks were added.

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: 23 passed. Not run: real
hardware.

## Fast step 2 results: kernel output and memory reuse (version 0.45.0)

Design and the full table: docs/f2-design.md.

**Measured** (`--skip-model --accel whpx`, `linux-guillemot`, kcycles
per child):
- fork + exit + wait: 9430–12476 before, 1535–2530 after (nine runs).
- `posix_spawn` + wait: 8201–8746 before, 1978–3156 after.
- Kernel's first line → `SHELL ready` (`fast-boot`): 166–174 ms before;
  after, mostly 109–112 ms, with some runs near 170.

The probes that found the causes were temporary and are removed:
- One log line cost 2.2–4.3 Mcycles.
- The first write to never-used frames cost 2.6–3.3 Mcycles per fork,
  against 0.1 for the copy itself.

`guillemot-test` now forces its 4 MiB buffer to be real. The compiler had
removed it, which made L5's 4 MiB timing invalid.

**Full `cargo xtask verify --skip-model`** (TCG): exit 0, 44 cases `OK`,
`assistant-live` `SKIP`. Then `--accel whpx --case assistant-live`: pass in
36.7 s.

**Negative controls** (temporary edits, restored and compared):

| Change | Result |
| --- | --- |
| The console is never redrawn after scrolling | `FAIL scroll`: row 40 does not show `zoo> echo end` |
| Buffered serial output never sent | `FAIL normal` (timeout): no `ZOO_OS BOOT` within 30 s |
| A reused frame is not marked taken | `FAIL normal`: `PANIC self-test frames: free count not restored` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. Not run: real hardware, which has no emulator exits but has
real UART timing and video memory.

## Linux step 5 results: processes and pipes (version 0.44.0)

Design: docs/linux5-design.md. New case `linux-guillemot`:
`guillemot-test` (static musl C) forks, waits, pipes, `dup2`s,
`execvp`s, `posix_spawn`s, kills and times. The case requires all of its
120 children's process lines in order, with their exit codes and signals.

**Measured** (`--skip-model --accel whpx --case linux-guillemot`, three
runs, averages per child): fork + exit + wait 12476 / 9452 / 9430
kcycles; with 4 MiB touched 9199 / 9438 / 9710; `posix_spawn` + wait
8746 / 8637 / 8201.

**Where that time goes** (temporary probes, removed):
- `fork()` alone took 7242 kcycles, `waitpid` 2461, and a plain system
  call 0.
- One kernel `ZOO_OS PROC` line took 2168–4315 kcycles (polled serial plus
  console scroll).
- Without the child's home-folder session: 10189 / 10552 (no change).

Making kernel output cheap is F2.

**Full `cargo xtask verify --skip-model`** (TCG): exit 0, 44 cases `OK`,
`assistant-live` `SKIP`. Then `--accel whpx --case assistant-live`: pass in
39.1 s.

**Negative controls** (temporary edits to `kernel/src/process.rs`,
restored and compared with `cmp`):

| Change | Result |
| --- | --- |
| The child does not get 0 from `fork` | `FAIL`: `exit pid=27 … code=11` while expecting `code=7` |
| Closing a pipe's write end is not counted | `FAIL` (timeout): no `start pid=30` within 120 s |
| `SIGPIPE` ignored by default | `FAIL`: `exit pid=34 … code=0` while expecting `signal=13`; program exit 71 |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: 23 passed. Not run: real
hardware.

## Linux step 4 results: writable files in the home folder (version 0.43.0)

Design: docs/linux4-design.md. New case `linux-fulmar`: `fulmar-test`
(static musl C) makes a folder, writes, appends, renames, lists,
unlinks, truncates and times 1 MiB in `/home/user`; ZooOS's shell then
`cat`s the file it left (`line one`, `line two`).

**Measured** (`cargo xtask verify --skip-model --accel whpx --case
linux-fulmar`, three runs each, the program's own `rdtsc` timing):

| 1 MiB | Message path (shared buffer disabled) | Shared buffer |
| --- | --- | --- |
| Write | 92437 / 77554 / 80497 kcycles | 6820 / 7959 / 6426 kcycles |
| Read | 57420 / 47356 / 44674 kcycles | 833 / 884 / 733 kcycles |

**Full `cargo xtask verify --skip-model`** (TCG): exit 0, 43 cases `OK`
and `assistant-live` `SKIP` (it needs an accelerator); `linux-fulmar`
passed in 9.3 s. Then `--accel whpx --case assistant-live`: pass in 33.7 s.

**Negative controls** (temporary edits to `kernel/src/process.rs`, restored
and compared with `cmp`):

| Change | Result |
| --- | --- |
| Earlier replies of a call not kept (no replay) | `FAIL linux-fulmar` (timeout) |
| `O_APPEND` ignored | `FAIL linux-fulmar`: `FAILED read back`, exit 24 |
| Bulk write leaves its last byte stale while reporting it written | `FAIL linux-fulmar`: `FAILED read back`, exit 24 |

(The first version of the third control wrote one byte less and said so.
The program wrote the rest, as Linux allows, and the case passed. That is
correct behaviour, so the control was changed.)

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: 23 passed. Not run: real hardware.

## Fast files step 1 results: shared-memory bulk transfers (version 0.42.0)

Design and measurements: docs/f1-design.md.

**Benchmark** (`cargo xtask verify --skip-model --accel whpx --case
file-speed`, `bowhead-test`, 1 MiB through a private BadgerHandler):

| | Before (4 runs) | After (9 runs) |
| --- | --- | --- |
| Read | 40–107 Mcycles | 0.37–0.59 Mcycles |
| Write, file growing | 123–133 Mcycles | 6.9–7.8 Mcycles |
| Write again (warm) | — | 0.31–0.57 Mcycles |

The benchmark also exposed a scheduler stall (some runs one 10 ms tick
slower): the timer gave the CPU to the idle kernel task while a program
was ready. Fixed in this step; no stall in the runs after it.

**Full `cargo xtask verify`** (TCG): exit 0 in 11 min 17 s (earlier runs
today: 12–17 min), every case as expected (`assistant-live` `SKIP`); the
boot checks include the new shared-memory check in `ptarmigan-test`, and
`TEST process-cleanup ok` confirms its pages were freed. New case
`file-speed`.

Then `--accel whpx --case assistant-live`: pass in 32.8 s (earlier runs
today: 48–68 s). The shorter suite and live times came after the
scheduler fix; they are observations, not a controlled measurement.

**Negative controls** (temporary edits, restored and compared):

| Change | Result |
| --- | --- |
| BadgerHandler's bulk read one byte short | `FAIL file-speed`: the boot check `arcticfox-test` (reads over 2 KiB) already exits 41 |
| Shared pages never freed | `FAIL normal`: `PANIC self-test process-cleanup: 520754 frames free, expected 520790` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `zoo-user`,
`xtask`, `zoo-abi`): 0 warnings. `cargo test -p xtask -p zoo-abi
--locked`: all passed. Not run: real hardware.

## Linux step 3 results: folders, working directory, uname, environment (version 0.41.0)

Design: docs/linux3-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 15 min 10 s, every case as
expected (`assistant-live` `SKIP`). New case `linux-skua`: `uname Linux
x86_64`, `HOME=/home/user USER=user`, `/home/user: Documents Music
Photos`, `cwd /home/user; Documents/notes.txt starts "Shopping: flour,
eggs, milk."`, `all checks passed`, exit 0, no unsupported calls (a first
run showed `fcntl`, then added).

Then, under WHPX: `assistant-live` pass in 48.8 s; `linux-skua` pass in
3.8 s.

**Negative controls** (temporary edits to `kernel/src/process.rs`,
restored):

| Change | Result |
| --- | --- |
| No `.` and `..` in listings | `FAIL linux-skua`: `FAILED readdir . and ..`, exit 21 |
| `..` not applied in paths | `FAIL linux-skua`: `FAILED chdir ..`, exit 34 |
| Empty environment | `FAIL linux-skua`: `FAILED environment`, exit 11 |

The first attempt at the first control did not apply its edit (`cargo
fmt` had reformatted the line) and so passed; it was rerun with the edit
applied and failed as above.

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: real
hardware.

## Linux step 2 results: memory, read-only files and time (version 0.40.0)

Design: docs/linux2-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 15 min 59 s, every case as
expected (`assistant-live` `SKIP`). New case `linux-eider` (7.2 s):
`EiderTest: memory ok`, `/etc/services 180 bytes, 5 lines`, `slept 200
ms`, `unix=…` 0 s from the host clock, `all checks passed`, exit 0; no
`unsupported syscall` line (a first run showed `nr=204`,
`sched_getaffinity`, which was then added). `linux-hello` unchanged.

Then, under WHPX: `assistant-live` pass in 58.1 s; `linux-eider` pass in
4.3 s.

**Negative controls** (temporary edits to `kernel/src/process.rs`, each
restored and checked byte for byte):

| Change | Result |
| --- | --- |
| `mmap` always fails | `FAIL linux-eider`: `FAILED small malloc (errno 12)`, exit 10 |
| `fstat` size one byte too big | `FAIL linux-eider`: `FAILED read size`, exit 24 |
| `nanosleep` returns at once | `FAIL linux-eider`: `FAILED slept too little`, exit 32 |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: real
hardware.

## Linux step 1 results: a static musl program runs unmodified (version 0.39.0)

Design: docs/linux1-design.md (§5 as implemented). Owner decision: route
3 in docs/feasibility/libc-route.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 12 min 38 s, every case as
expected (`assistant-live` `SKIP`). New case `linux-hello` (6.0 s):
`linux hello alpha` → `PROC start pid=26 name=hello`, `[26:hello] Hello
from Linux musl: argc=2 argv[0]=hello argv[1]=alpha`, `PROC exit pid=26
name=hello code=0`; no `unsupported syscall` line. The program is built
with `zig cc -target x86_64-linux-musl -static` from plain C with
`printf`.

Then, under WHPX: `assistant-live` pass in 49.3 s; `linux-hello` pass in
4.0 s.

**Negative controls** (temporary kernel edits, reverted):

| Change | Result |
| --- | --- |
| `writev` returns `ENOSYS` | `FAIL linux-hello` (timeout): nothing printed |
| argc written one too high in the start-up stack | `FAIL linux-hello` (timeout): musl printed `argc=3 argv[0]=hello argv[1]=alpha argv[2]=(null)` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: real
hardware.

## Desktop step 10 results: a cursor in Notes, and a start-up race fixed (version 0.38.0)

Design: docs/d10-design.md.

**A race found on the way.** The first two full runs of this step each
failed one case that the step does not touch: `persist-3` (timeout) and
then `keyboard` (screen check). Same cause: BadgerHandler loads the disk
after it starts, and Zookeeper reported `ready` before that, so the shell
prompt could appear first and the service's messages printed after it
(`zoo> [23:badger-handler] …`; the typed line then had no prompt).
Fixes, in this step:

1. Zookeeper sends `ready` only after the home-folder root arrives (at
   most 300 ticks). Its first version checked at the top of its loop and
   so waited for the next 1 s health ping: fast boot measured 1,130 /
   1,127 / 1,124 ms kernel-to-shell. Checking after each round's
   messages restored 107 / 111 / 107 ms (WHPX, three runs each).
2. The harness gained `WaitSeen` (a line anywhere in the log) for the
   load lines in `persist-2`, `persist-3` and `notes-2`, which no longer
   assume an order against `SHELL ready`. A check that it still fails
   when the line never comes (a temporary wait for "generation 9"):
   `FAIL persist-2` (timeout).

**Full `cargo xtask verify`** (TCG, with both fixes): exit 0 in 12 min 13 s, every case
as expected (`assistant-live` `SKIP`). New case
`notes-cursor`: `a b c`, Left, Left, `X`, `Y`, Backspace → `Notes: saved
70 bytes`; the screen shows `aX|bc`.

Then `--accel whpx --case assistant-live`: pass in 48.0 s.

**Negative control** (temporary edit to Notes, reverted):

| Change | Result |
| --- | --- |
| Left ignored | `FAIL notes-cursor`: `aX|bc not on screen` |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: real hardware.

## Desktop step 9 results: double-click to zoom (version 0.37.0)

Design: docs/d9-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 12 min 8 s, every case as
expected (`assistant-live` `SKIP`). New case `zoom` (9.9 s): double-click
on the Assistant's title → `zoomed Assistant`, its title at (32, 52), the
other titles covered, close button at (1242, 60); double-click again →
`restored Assistant`, title back at (660, 416), Files visible.

Then `--accel whpx --case assistant-live`: pass in 49.4 s.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Double-clicks never detected | `FAIL zoom` (timeout: no `zoomed` line) |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: real hardware.

## Desktop step 8 results: resize windows (version 0.36.0)

Design: docs/d8-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 12 min 40 s, every case as
expected (`assistant-live` `SKIP`). New case `resize` (8.7 s): Crash
Test's corner dragged by (−200, −100) → `resized Crash Test to 404x240`;
screenshot: focused, its text shown, close button at (1042, 60) and not at
(1242, 60), desktop colour at (900, 300) below the new bottom edge.

Then `--accel whpx --case assistant-live`: pass in 49.0 s.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Resize corner never grabs | `FAIL resize` (timeout: no `resized` line) |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: real hardware.

## Clean-copy verification of version 0.35.0 (2026-10-31)

Every step's record so far said "Not run: a clean-copy verify". This run
closes that for the current state: a fresh `git clone` of `main` at
`d6d93d3` (merge of hardware step 5) into an empty directory, with only
the two Git-ignored downloads copied in from the development tree (the
pinned Zig 0.16.0 toolchain, `tools/zig/zig-x86_64-windows-0.16.0`, and
the checksum-pinned model file in `tools/models/`), then:

| Command | Result |
| --- | --- |
| `cargo xtask doctor` | `ready: all prerequisites found` |
| `cargo xtask verify` (TCG, everything built from nothing) | exit 0 in 15 min 56 s; 35 cases as expected, `assistant-live` `SKIP`; `PASS` |
| `cargo xtask verify --accel whpx --case assistant-live` | pass in 50.6 s |

The clone's working tree stayed clean (no tracked file changed by the
build). So the merged state builds and verifies from Git plus the
documented downloads alone.

## Hardware step 5 results: key repeat for USB keyboards (version 0.35.0)

Design: docs/h5-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 35 s, every case as
expected (`assistant-live` `SKIP`). New case `usb-repeat` (9.3 s): `x`
held for 1.5 s through QMP → `SHELL line=` with 34 x's (at least ten
required; the quiet wait after release proves the repeat stopped).

Then `--accel whpx --case assistant-live`: pass in 51.2 s.

**Negative controls** (temporary edits to `kernel/src/xhci.rs`, reverted):

| Change | Result |
| --- | --- |
| No repeat | `FAIL usb-repeat`: `got ZOO_OS SHELL line=x while expecting ZOO_OS SHELL line=xxxxxxxxxx…` |
| Repeat not stopped on release | `FAIL usb-repeat`: Enter kept repeating, `got ZOO_OS SHELL line= after the final expected marker` |

Other checks: `cargo clippy` (kernel, `xtask`): 0 warnings. `cargo test
-p xtask --locked`: all passed. Not run: real hardware.

## Hardware step 4 results: USB hubs (version 0.34.0)

Design: docs/h4-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 58 s, every case as
expected (`assistant-live` `SKIP`). New case `usb-hub` (7.8 s): `port=5
vendor=0x0409 product=0x55aa class=9` (QEMU's hub), `port=5.1
hid=keyboard … input ready`, `port=5.2 hid=mouse … input ready`,
`devices=3`; `echo hub` typed through the keyboard behind the hub →
`SHELL line=echo hub`. `usb` and `usb-desktop` pass unchanged.

Then `--accel whpx --case assistant-live`: pass in 50.1 s.

**Negative control** (temporary edit to `kernel/src/xhci.rs`, reverted):

| Change | Result |
| --- | --- |
| Hub port left out of the route string | `FAIL usb-hub` (timeout): `port=5.1 error=command failed`, `port=5.2 error=command failed`, `devices=1` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: a
clean-copy verify; real hardware.

## Hardware step 3 results: the local APIC timer (version 0.33.0)

Design: docs/h3-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 13 min 24 s, every case as
expected (`assistant-live` `SKIP`), all on `timer source=lapic
pm-timer=0x608`. New case `timer-rate`: `TernTest: waited` came 4,964 ms
after `waiting 500 ticks` (4–6 s required); 4,965 ms and, under WHPX,
4,958 ms in the earlier single runs. `fast-boot` under WHPX: pass with
`source=lapic`.

Then `--accel whpx --case assistant-live`: pass in 58.5 s
(`source=lapic per-tick=125071`).

PIT fallback (a temporary edit forcing it): `timer-rate` 4,958 ms and
`desktop` pass with `source=pit reason=forced-for-test`.

**Negative control** (temporary edit to `kernel/src/apic.rs`, reverted):

| Change | Result |
| --- | --- |
| Calibration doubled | `FAIL timer-rate` (`harness-error`): `waited came 10012 ms after the previous line (expected 4000-6000 ms)` |

Other checks: `cargo clippy` (kernel normal, `fast-boot`, `panic-test`;
`xtask`): 0 warnings. `cargo test -p xtask --locked`: all passed. Not
run: a clean-copy verify; real hardware.

## Hardware step 2a results: ready for a USB stick (version 0.32.0)

Plan and risks: docs/usb-boot.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 31 s, every case as
expected (`assistant-live` `SKIP`). New case `big-screen` (9.0 s, test-only
mode whose boot configuration asks for a screen at least 2000 pixels
wide): QEMU gave 2560×1600; `PeacockHandler: display 2560x1600: using
1920x1200`; the desktop checks pass and the area right of x=1920 is black.

`cargo xtask usb-image`: `build/usb/zoo-os-usb.img (3211264 bytes)` with
its SHA-256 printed; no device written.

Then `--accel whpx --case assistant-live`: pass in 63.0 s.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Large screens refused, as before | `FAIL big-screen`: `got ZOO_OS PROC exit pid=27 name=peacock-handler code=3 while expecting ... files` |

Other checks: `cargo clippy` (kernel normal, `fast-boot`, `big-screen`;
`zoo-user`; `xtask`): 0 warnings. A first version set the screen request
in the kernel's bootloader configuration, which is deprecated in
bootloader 0.11; it moved to the disk image's boot configuration. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## Hardware step 1b results: USB keyboard and mouse input (version 0.31.0)

Design: docs/h1b-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 17 min 24 s, every case as
expected (`assistant-live` `SKIP`).

| Case | Time | What it shows |
| --- | --- | --- |
| usb | 8.9 s | both devices `input ready`; `echo usb` typed → `usb keyboard input`, `SHELL line=echo usb` |
| usb-desktop | 11.7 s | `desktop` typed through the USB keyboard; a USB mouse move and click → `usb mouse input`, `click focuses Crash Test`; screenshot: Crash Test focused, Files not |

Then `--accel whpx --case assistant-live`: pass in 58.1 s.

**Negative controls** (temporary edits to `kernel/src/xhci.rs`, reverted):

| Change | Result |
| --- | --- |
| USB key presses not queued | `FAIL usb` (timeout): `no ZOO_OS SHELL line=echo usb within 30s`; so the keys did not reach the PS/2 keyboard either |
| USB mouse y not negated | `FAIL usb-desktop` (timeout): the click landed on the Assistant (`click focuses Assistant`) |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: a
clean-copy verify; real hardware.

## Hardware step 1a results: USB host controller, enumeration (version 0.30.0)

Design and details: docs/h1-design.md.

**Full `cargo xtask verify`** (TCG): exit 0 in 15 min 18 s, every case as
expected (`assistant-live` `SKIP`); boots without a USB controller log
`ZOO_OS INFO usb none`. New case `usb` (6.2 s, with `qemu-xhci`,
`usb-kbd`, `usb-mouse`): `usb xhci ports=8 slots=32 context=32`; port 5
`hid=keyboard`, `vendor=0x0627 product=0x0001 class=3`; port 6
`hid=mouse`, same ids; `usb devices=2`; then `SHELL ready`.

Then `--accel whpx --case assistant-live`: pass in 68.0 s.

Boot time (fast-boot, WHPX, kernel's first line → `SHELL ready`, seven
runs): 110, 168, 160, 110, 117, 109, 112 ms (before: 108–110 ms). A first
version that scanned all 256 PCI buses measured 226, 216, 282 ms and was
replaced by a bridge walk.

**Negative controls** (temporary edits to `kernel/src/xhci.rs`, reverted):

| Change | Result |
| --- | --- |
| Event ring read with the wrong cycle bit | `FAIL usb` (timeout): `port=5 error=command timed out`, `port=6 error=command timed out`, `devices=0` |
| USB 2 port reset skipped | passed: QEMU enables USB 2 ports without a reset, so this path is not tested |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: a
clean-copy verify; real hardware.

## Desktop step 7 results: Files browses the home folder (version 0.29.0)

Design: docs/d7-design.md (§4 as implemented).

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 49 s, every case as
expected (`assistant-live` `SKIP`). Changed checks: `desktop` (`Home` with
`Documents/` selected; Down, Down, Enter opens `Home/Photos` with
`README.txt (34 bytes)` selected; kept through Crash Test's restart; reset
after the window server's restart), `fast-boot`, and `assistant-model`'s
busy-desktop check (`Photos/` selected while the model works). New case
`files-browse`: the preview of `Home/Documents/notes.txt` shows its lines
(no stray `?`); after going into `Photos` and back up, `Photos/` is
selected.

Then `--accel whpx --case assistant-live`: pass in 57.7 s.

**Negative control** (temporary edit to Files, reverted):

| Change | Result |
| --- | --- |
| Going up forgets the folder you came from | `FAIL files-browse`: `Photos/ not on screen` (as the selection) |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## Desktop step 6 results: start in the desktop (version 0.28.0)

Design: docs/d6-design.md (§3 as implemented).

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 24 s, every case as
expected (`assistant-live` `SKIP`). The `fast-boot` case (7.5 s) now
shows `ZOO_OS SHELL autostart desktop`, the session's processes (pids
5–9) and the display, a desktop screenshot passing `check_desktop_started`;
after Escape, `run tern-test fast` (pid 10) exits with code 0.

Then `--accel whpx --case assistant-live`: pass in 57.1 s.

**Negative control** (temporary edit to the kernel shell, reverted):

| Change | Result |
| --- | --- |
| No autostart | `FAIL fast-boot` (timeout): `no ZOO_OS SHELL autostart desktop within 30s` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings. `cargo test -p xtask --locked`: all passed. Not run: a
clean-copy verify; real hardware.

## Desktop step 5 results: date and time in the menu bar (version 0.27.0)

Design: docs/d5-design.md (§3 as implemented).

**Full `cargo xtask verify`** (TCG): exit 0 in 14 min 18 s, every case as
expected (`assistant-live` `SKIP`). New case `menu-clock`: the menu bar
showed `Sat 31 Oct 06:33 UTC`, the host's UTC time of the screenshot. New
unit test `menu_clock_text_known_dates` (1970-01-01 Thursday, 2000-02-29
Tuesday, 2026-10-31 Saturday): passed.

Then `--accel whpx --case assistant-live`: pass in 60.4 s.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Date one day ahead | `FAIL menu-clock`: `menu bar does not show Sat 31 Oct 06:29 UTC or Sat 31 Oct 06:28 UTC` |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## Boot step 1 results: fast boot (version 0.26.0)

Design and measurements: docs/b1-design.md. Kernel feature and image mode
`fast-boot` (no boot-check programs); `serial-times.log` per run.

**Full `cargo xtask verify`** (TCG): exit 0 in 12 min 31 s, every case as
expected (`assistant-live` `SKIP`); all existing cases still use the normal
build with every check. New case `fast-boot` (4.5 s): self-tests, then
`PROC start pid=1 name=zookeeper` (supervise mode) with BadgerHandler,
BeaverHandler and SealTest as pids 2–4, `READY`, `SHELL ready`, and `run
tern-test fast` → `argc=2 argv=tern-test,fast`, exit code 0.

Then `--accel whpx --case assistant-live`: pass in 62.2 s.

**Measurement** (`--skip-model --accel whpx`, three runs each), kernel's
first line → `SHELL ready`: normal 1,072 / 1,058 / 1,075 ms; fast-boot 108
/ 108 / 110 ms. QEMU start → `SHELL ready`: normal 2,090 / 1,973 / 2,099
ms; fast-boot 1,136 / 1,284 / 1,238 ms (the rest is UEFI firmware).

**Negative controls:**

| Change | Result |
| --- | --- |
| `fast-boot` condition forced false (checks run anyway) | `FAIL fast-boot`: `got ZOO_OS PROC start pid=2 name=beluga-test while expecting ... badger-handler` |
| Two mode features enabled together (`fast-boot,panic-test`) | the kernel does not compile: `at most one mode feature may be enabled` |

Other checks: `cargo clippy` (kernel normal and `fast-boot`, `xtask`): 0
warnings (one lint allowed on the mode-count assertion, which is always
true in a build that compiles). `cargo test -p xtask --locked`: all
passed. Not run: a clean-copy verify; real hardware.

## Desktop step 4 results: Notes (version 0.25.0)

Design: docs/d4-design.md (§4 as implemented).

**Full `cargo xtask verify`** (TCG): exit 0 in 13 min 35 s, every case as
expected (`assistant-live` `SKIP`), the dock case on the four-tile layout.
New cases, two boots of one data disk:

| Case | Time | What it shows |
| --- | --- | --- |
| notes-1 | 12.6 s | Notes opened from the dock (`PROC start pid=31 name=notes`); `Hello from Notes` and `second line` typed; `Notes: saved`, then `BadgerHandler: saved generation 1 to slot A`; screen: `(saved)`, the original two lines without stray `?`, then the typed lines in order |
| notes-2 | 9.8 s | reboot: `loaded generation 1 from slot A`; Notes shows the typed lines again |

Then `--accel whpx --case assistant-live`: pass in 61.5 s.

**Negative controls** (temporary edits to Notes, reverted):

| Change | Result |
| --- | --- |
| No save on the timer | `FAIL notes-1` (timeout waiting for `Notes: saved`); `FAIL notes-2` (timeout: nothing to load) |
| CRLF not handled (the first version's bug) | `FAIL notes-1`: `wrong text Shopping: flour, eggs, milk.? matched at (208, 152)` |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## POSIX step 5b-1 results: process arguments and the clock (version 0.24.0)

Design: docs/posix5b1-design.md (§4 as implemented). Command lines in
`spawn`, calls 57 `args_get` and 58 `clock` (ABI version 16), CMOS RTC at
boot (`ZOO_OS INFO clock unix=…`), C program `tern-test`, harness action
`WaitClock`.

**Full `cargo xtask verify`** (TCG): exit 0 in 13 min 0 s, every case as
expected (`assistant-live` `SKIP`). New case `args-clock` (7.4 s): `run
tern-test alpha beta` → `TernTest: argc=3 argv=tern-test,alpha,beta`;
`TernTest: unix=…` 0 s from the host clock (allowed 120 s); `TernTest:
monotonic +500 ms` across a 50-tick sleep; exit code 0.

Then `--accel whpx --case assistant-live`: pass in 61.2 s.

**Negative controls** (each temporary kernel edit reverted):

| Change | Result |
| --- | --- |
| RTC values not BCD-decoded | `FAIL args-clock`: the date read as impossible, the clock stayed unset and TernTest exited with code 12 (`unexpected-marker`) |
| Year read as 1999 + YY (a plausible wrong time) | `FAIL args-clock` (`harness-error`): `guest clock 1761888157 is 31536001 s from the host's 1793424158 (allowed 120 s)` |
| Command line not stored (arguments dropped) | `FAIL args-clock` (timeout): the program printed `argc=1 argv=tern-test` |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`):
0 warnings (one unnecessary cast fixed). `cargo test -p zoo-infer -p
zoo-abi -p xtask --locked`: all passed. Not run: a clean-copy verify;
real hardware (the RTC is QEMU's).

## Desktop step 3 results: a dock and close buttons (version 0.23.0)

Design: docs/d3-design.md (§4 as implemented).

**Full `cargo xtask verify`** (TCG): exit 0 in 15 min 6 s, every case as
expected (`assistant-live` `SKIP`); existing desktop, mouse, redraw and
assistant checks unchanged. New case `dock` (13.4 s):

| Screenshot | Check |
| --- | --- |
| 1-dock | pointer at the bottom: tiles `Files`, `Crash Test`, `Assistant` at y 736, three running dots |
| 2-closed | after Crash Test's close button: its window and text gone, `Files` still focused, dock hidden; serial `PROC exit pid=29 name=crash-test code=0` (no restart) |
| 3-dock-closed | no running dot under Crash Test; dots under Files and the Assistant |
| 4-reopened | after its tile: `PROC start pid=31 name=crash-test`, window back and focused, dock hidden |

Then `--accel whpx --case assistant-live`: pass in 61.9 s.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Close restarts the app instead of closing it | `FAIL dock` (timeout, 60 s): the app never closed, so its dock tile raised it instead of opening it and `opened Crash Test` never appeared |

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## Desktop step 2 results: redraw only what changed (version 0.22.0)

Design: docs/d2-design.md (§5 as implemented, with the measurements).
PeacockHandler damage rectangles, clipped drawing, partial present; test
keys F9/F10/F11.

**Full `cargo xtask verify`** (TCG): exit 0 in 10 min 48 s, every case as
expected (`assistant-live` `SKIP`). The existing desktop, mouse and
assistant screen checks pass unchanged. New cases:

| Case | Result |
| --- | --- |
| redraw | After a click, a drag of Files to (124, 98), Tab and eight pointer moves, the screen equals a forced full repaint: 993,280 pixels compared (menu bar excluded), 0 differ |
| frames | Both timing reports logged (cycle counts under TCG are not meaningful) |

Then `--accel whpx --case assistant-live`: pass in 51.0 s.

**Measurement** (`cargo xtask verify --skip-model --accel whpx --case
frames`, three runs): median pointer-move frame 4,342,631 / 4,039,118 /
4,176,248 cycles with full repaints (1,024,000 px) versus 31,610 / 31,158 /
31,718 cycles with partial redraws (408 px). Details and limits:
docs/d2-design.md §5.

**Negative control** (temporary edit to PeacockHandler, reverted):

| Change | Result |
| --- | --- |
| Pointer's old box not damaged | `FAIL redraw`: `405 of 993280 pixels differ from the partial redraws, first at (400, 110)` |

The first version of `redraw` did not catch this control (it passed): the
focus change after the pointer moves caused a full repaint that cleaned up
the stale arrows. The case now moves the pointer last; the control was
rerun and failed as above.

Other checks: `cargo clippy` (`zoo-user`, `xtask`): 0 warnings. `cargo
test -p xtask --locked`: all passed. Not run: a clean-copy verify; real
hardware.

## Desktop step 1 results: a mouse (version 0.21.0)

Design: docs/d1-design.md (§4 as implemented). PS/2 mouse on IRQ12, call 56
`mouse_read` (ABI version 15); PeacockHandler pointer, click to focus,
title-bar drag. Every boot logs `ZOO_OS INFO mouse ps2=true`.

**Full `cargo xtask verify`** (TCG): exit 0 in 12 min 49 s, every case as
expected (`assistant-live` `SKIP`), including the new case:

| Case | Time | Screen checks |
| --- | --- | --- |
| mouse | 10.4 s | `1-pointer`: arrow at (640, 400), `Files` focused. `2-click`: after moving to (840, 200) and one click, `Crash Test` focused (title and menu bar), `Files` not. `3-drag`: the Assistant title bar dragged by (-300, -100); its title found at (360, 316) instead of (660, 416), log `moved Assistant to (352, 312)` |

Then `--accel whpx --case assistant-live`: pass in 61.5 s.

**Negative control** (temporary edit to `kernel/src/mouse.rs`, reverted):

| Change | Result |
| --- | --- |
| Packet sign bits ignored (negative movement becomes large positive) | `FAIL mouse` (timeout, 60 s): the click never landed on Crash Test, so `click focuses Crash Test` never appeared |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed. Not run: a clean-copy verify; a real mouse on real
hardware (QEMU only).

## Services step 5 results: files survive a reboot (version 0.20.0)

Design: docs/s5-design.md (§5 as implemented). Kernel virtio block driver
(`kernel/src/virtio_blk.rs`, legacy interface, polled) and calls 52–55 (ABI
version 14); a 16 MiB data disk on every QEMU run; BadgerHandler A/B
snapshots with generation and CRC-32; shell `dir`/`cat`/`put`.

**Full `cargo xtask verify`** (TCG): exit 0 in 805 s, every case as
expected (`assistant-live` `SKIP`), with the three new boots of one disk:

| Case | Disk | Time | What it shows |
| --- | --- | --- | --- |
| persist-1 | fresh | 14.2 s | `put` twice → `saved generation 1 to slot A`, `saved generation 2 to slot B`; `cat` shows `second version` |
| persist-2 | same (reboot) | 10.8 s | `loaded generation 2 from slot B (457 bytes)`; `cat` shows `second version`; `dir Documents` lists `kept.txt (15 bytes)` |
| persist-3 | same, generation 2 damaged by xtask (`flipped byte 8392724`) | 17.0 s | `slot B: generation 2 failed its check, ignored`, `loaded generation 1 from slot A`; then a save cut short (`stopping after the payload of generation 2 (simulated power loss)`, exit 41), the supervisor restarts BadgerHandler and generation 1 loads again |

Then `--accel whpx --case assistant-live`: pass in 58.3 s. Every boot
reports `ZOO_OS INFO disk virtio-blk bytes=16777216`.

**Negative controls** (each temporary edit to BadgerHandler reverted):

| Change | Result |
| --- | --- |
| CRC not checked on load | `persist-1`, `persist-2` OK; `FAIL persist-3`: the damaged generation 2 was loaded (`loaded generation 2 from slot B`, then `snapshot did not parse; using the seed`) |
| First valid slot taken instead of the newest | `FAIL persist-2`: `loaded generation 1 from slot A` |

The planned control "header before payload" was not run: with the CRC it
cannot fail (docs/s5-design.md §5 explains why).

Other checks: `cargo clippy`: 0 warnings (one `too_many_arguments` allowed
on `run_case`). `cargo test -p zoo-infer -p zoo-abi -p xtask --locked`: all
passed. Not run: a clean-copy verify.

## Services step 4b results: the filesystem as a service (version 0.19.0)

Design: docs/s4-design.md (§6 as implemented). BadgerHandler holds the file
tree; folder handles are channels to it (`user/src/fsproto.rs`); the
kernel's filesystem and calls 19–25, 41–45 are gone (ABI version 13);
handle and `wait_any` limits 64; `restart <service>`.

**Full `cargo xtask verify`** (TCG): exit 0 in 679 s, every case as expected
(`assistant-live` `SKIP`; `services` OK in 26.1 s). Then `--accel whpx
--case assistant-live`: pass in 59.6 s. Every earlier file test passed
unchanged on BadgerHandler:

    ZOO_OS PROC start pid=8 name=badger-handler
    [9:arcticfox-test] ArcticFoxTest: all file checks passed
    [1:zookeeper] OtterHandler answered 19 scripted requests exactly
    ZOO_OS PROC exit pid=8 name=badger-handler code=0
    ...
    ZOO_OS PROC start pid=23 name=badger-handler        (resident, supervised)

and the `desktop`, `assistant` and `assistant-model` cases (Files, the
Assistant's broker) passed with the desktop's home folder now coming from
the supervisor. Zookeeper also checks that the old `fs_root` call returns
`NOSYS`. From the `services` case:

    ZOO_OS PROC killed pid=23 name=badger-handler by=22
    [22:zookeeper] Zookeeper: badger-handler killed; restarting (1 of 3)
    ZOO_OS PROC start pid=30 name=badger-handler
    [22:zookeeper] Zookeeper: filesystem root from badger-handler (pid 30)
    badger-handler pid=30 running restarts=1 last=killed

**Negative controls** (normal case; each temporary edit to BadgerHandler
reverted):

| Change | Result |
| --- | --- |
| Rights not checked | `FAIL normal`: `zookeeper code=26`, `create via read-only handle: expected RIGHTS, got Ok(())` |
| `..` accepted as a path component | `FAIL normal`: `zookeeper code=26`, `dir_open ../bin: expected INVAL, got Err(NotFound)` |

Not automated: a client holding a folder handle while BadgerHandler dies.
Other checks: `cargo clippy`: 0 warnings. `cargo test -p zoo-infer -p
zoo-abi -p xtask --locked`: all passed. Not run: a clean-copy verify.

## Services step 4a results: wait_any and faster hand-offs (version 0.18.0)

Design: docs/s4a-design.md. Call 51 `wait_any` (ABI version 12); Zookeeper
and BeaverHandler wait on channels instead of sleeping; the scheduler
prefers a ready user task over the idling kernel task on voluntary
switches; harness `WaitFor` accepts lines after shell prompts; the shell
waits for a desktop session's apps before reprinting its prompt.

**Measured** (seal-test, 50 ping round trips to BeaverHandler over a
channel it got by name, `services` case, TCG, this host):

| Version | 50 round trips |
| --- | --- |
| Before (sleep polling) | 250 ticks |
| `wait_any` only | 50 ticks |
| `wait_any` + scheduler preference | 0 ticks (in three runs) |

**Full `cargo xtask verify`** (TCG): exit 0 in 634 s, every case as
expected (`assistant-live` `SKIP`). Then `--accel whpx --case
assistant-live`: pass in 53.7 s. The first full run of this step failed one
check: the `desktop` case's last screenshot had an app's exit line after
the shell prompt (`zoo> ZOO_OS PROC exit pid=33 name=assistant code=0`),
because the faster scheduling let the prompt print before the session's
apps had exited. Fixed in the shell (§4 of the design); the rerun above
passed.

Also found while doing this step: with the scheduler change the `services`
case stalled because a kernel line shared a line with the prompt and the
harness's `WaitFor` needed whole-line equality. Diagnosed with temporary
scheduler logging (removed) that showed the kernel idle and every service
blocked, i.e. no starvation.

**Negative control:** `wait_any` not treating a closed peer as ready:
`FAIL normal`, `ptarmigan-test code=14`, `closed peer is ready`.

Other checks: `cargo clippy`: 0 warnings. `cargo test -p zoo-infer -p
zoo-abi -p xtask --locked`: all passed (new `find_line` test). Not run: a
clean-copy verify.

## Services step 3 results: handles in messages (version 0.17.0)

Design: docs/s3-design.md. Calls 49 `send_handle` and 50 `recv_handle`,
error 19 `WRONGKIND` (ABI version 11), Zookeeper's `connect <name>`,
BeaverHandler serving clients, seal-test's `connect`, and the boot test
`ptarmigan-test` (pid 20; later process ids moved up by one).

**Full `cargo xtask verify`** (TCG): exit 0 in 625 s, every case as
expected (`assistant-live` `SKIP`; `services` OK in 22.3 s). Then
`--accel whpx --case assistant-live`: pass in 54.6 s.

    [20:ptarmigan-test] PtarmiganTest: all handle-passing checks passed
    ...
    zoo> poke seal-test connect
    [21:zookeeper] Zookeeper: connected seal-test to beaver-handler
    [23:seal-test] seal-test: connected to beaver-handler by name; it returned 2 lines

ptarmigan-test checks: a moved handle is gone from the sender and works in
the receiver with its tag; `WRONGKIND` in both directions with the message
kept; an end cannot be sent through itself; a channel end queued towards a
closed end is released (its peer sees `PEERCLOSED`).

**Negative controls** (normal case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| `send_handle` copies instead of moving | `FAIL normal`: `ptarmigan-test code=10`, `moved handle gone from sender` |
| Queued handles not released when an end closes | `FAIL normal`: `ptarmigan-test code=13`, `queued handle released` |

Other checks: `cargo clippy`: 0 warnings (one collapsed `match` arm in
seal-test). `cargo test -p zoo-infer -p zoo-abi -p xtask --locked`: all
passed. Not run: a clean-copy verify.

## Services step 2 results: log routing (version 0.16.0)

Design: docs/s2-design.md. Kernel log buffer (`kernel/src/klog.rs`), calls
47 `log_attach` and 48 `log_read` (ABI version 10), BeaverHandler draining
the buffer, `logs [text]`.

**Full `cargo xtask verify`** (TCG): exit 0 in 561 s, every case as
expected (`assistant-live` `SKIP`; `services` OK in 20.1 s). Then
`--accel whpx --case assistant-live`: pass in 48.6 s. (In this run
`negative-timeout` again timed out waiting for `NEVER-EMITTED`; in the S1
run boot was slow enough that it timed out on an earlier marker. Either way
the timeout is rejected.)

From the `services` case:

    ZOO_OS SHELL line=logs puffin
    [    14.24] [26:puffin-test] PuffinTest: hello from C
    [    14.24] [26:puffin-test] PuffinTest: crc32 0x414fa339, sqrt(2) ok, 64 KiB heap crc32 0x85400450
    [    14.24] [26:puffin-test] PuffinTest: all C checks passed

and plain `logs` begins with `(32 earlier matching lines not shown)` followed
by boot-time lines from Zookeeper, showing output from every program.

**Negative control:** with `klog::push` removed, `FAIL services …
got=timeout`: the supervisor's events never reach BeaverHandler, so the
`logs` check before `run puffin-test` never matches.

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed. Not run: a clean-copy verify.

## Services step 1 results: resident supervisor (version 0.15.0)

Design: docs/s1-design.md. Zookeeper's supervise mode, BeaverHandler (log
service), seal-test (test service), `etc/services`, shell commands
`services`/`logs`/`poke`, call 46 `process_id` (ABI version 9), and the
harness action `WaitContains`. Every boot now starts the resident
Zookeeper (pid 20), BeaverHandler (21) and seal-test (22) before `READY`;
later process ids moved up by 3.

**Full `cargo xtask verify`** (TCG): exit 0 in 813 s, every case as expected
(`assistant-live` `SKIP`; new `services` case OK in 17.7 s). Then
`--accel whpx --case assistant-live`: pass in 63.7 s. The
`negative-timeout` case now times out while waiting for
`PROC start pid=17 name=wolverine-test` (boot takes longer than its 5 s
limit), so it still demonstrates a rejected timeout, just earlier.

From the `services` case (shell output and kernel markers):

    beaver-handler pid=21 running restarts=0 last=none
    seal-test pid=22 running restarts=0 last=none
    ZOO_OS PROC crash pid=22 name=seal-test fault=page-fault access=read cause=protection addr=0xffffc00000001000
    ZOO_OS PROC start pid=23 name=seal-test
    seal-test pid=23 running restarts=1 last=crashed
    ZOO_OS PROC killed pid=23 name=seal-test by=20
    ZOO_OS PROC start pid=24 name=seal-test
    seal-test pid=24 running restarts=2 last=unresponsive
    ...
    seal-test pid=25 failed restarts=3 last=crashed
    [     7.41] seal-test unresponsive; restarting (2 of 3)
    [    10.20] seal-test crashed; restart limit reached, left stopped

The hang was poked at about 3.8 s and the kill came at 7.41 s: within one
ping interval plus the 2 s answer limit.

**Negative controls** (`services` case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| Zookeeper never sends health pings | `FAIL services … got=timeout 90.0s`: no `PROC killed pid=23` (the hang is never noticed) |
| Restart limit off by one | `FAIL services`: `got ZOO_OS PROC start pid=26 name=seal-test while expecting ZOO_OS SHELL line=services` |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed. Not run: a clean-copy verify.

## Program names (version 0.14.0)

Naming convention set by the owner (DECISIONS.md, "Program names"): only
system services keep `…Handler`; apps get plain names; boot-time tests are
Arctic animals with a `-test` suffix. Behaviour is unchanged.

| Old | New |
| --- | --- |
| lion-handler | beluga-test |
| monkey-handler | lemming-test |
| hamster-handler | muskox-test |
| beaver-handler | narwhal-test |
| squirrel-handler | arcticfox-test |
| ant-handler | caribou-test |
| mole-handler | walrus-test |
| gecko-handler | wolverine-test |
| capybara-handler | puffin-test |
| owl-handler | files |
| parrot-handler | assistant |
| chimp-handler | crash-test (window title "Crash Test") |

**Full `cargo xtask verify`** (TCG): exit 0 in 717 s, every case as
expected (`assistant-live` `SKIP`), with the new names in every marker and
screen check, e.g.:

    screen 1-desktop: … selected `bin/zookeeper` at (32, 80), Crash Test window shown; wrong text rejected
    screen 2-details: details of the 3rd file: `Name: bin/lemming-test` at (32, 80), …
    screen 3-app-restart: status `Crash Test crashed; restarted (1 of 3)` at (8, 780), focused `Crash Test` at (660, 52), …

Then `--accel whpx --case assistant-live`: pass in 61.7 s.

## POSIX step 5a results: C toolchain (version 0.14.0)

Design: docs/posix5-design.md. Zig 0.16.0 downloaded into `tools/zig/`
with the owner's approval: `zig-x86_64-windows-0.16.0.zip`, 97,217,739
bytes, SHA-256 `68659eb5…a75a7e`, identical to ziglang.org's index
(tools/zig/README.md); `zig version` 0.16.0, `zig cc --version` clang
21.1.0. The initrd now holds 16 programs, one of them in C (610,816 bytes).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 812 s,
`ZOO_OS BOOT version=0.14.0`, every case as expected (`assistant-live`
`SKIP`). This run shared the CPU with an interactive demo session, so its
times are longer than usual (normal 6.7 s, desktop 16.5 s, inference
624.9 s). New case `c-program`: OK in 7.7 s. Then `--accel whpx --case
assistant-live`: pass in 53.5 s.

From the `c-program` case's serial log:

    ZOO_OS SHELL line=run capybara-handler
    ZOO_OS PROC start pid=20 name=capybara-handler
    [20:capybara-handler] CapybaraHandler: hello from C
    [20:capybara-handler] CapybaraHandler: crc32 0x414fa339, sqrt(2) ok, 64 KiB heap crc32 0x85400450
    [20:capybara-handler] CapybaraHandler: all C checks passed
    ZOO_OS PROC exit pid=20 name=capybara-handler code=0

**Negative and absence checks** (each reverted):

| Change | Result |
| --- | --- |
| CapybaraHandler expects a wrong CRC-32 value | `FAIL c-program`: `capybara-handler code=11` |
| Toolchain folder moved aside | `[build] no C toolchain in tools/zig/ …; C programs skipped`, initrd with 15 programs, `SKIP c-program not run: no C toolchain` |

**Interactive demo** (not a verify case, recorded as an observation): the
model image under `--accel whpx` in a visible window, driven by keystrokes
over QMP: boot checks, shell commands, the C program, the desktop, and the
Assistant answering "Find my pancake recipe" (`search pancake recipe`) and
"Make a folder called Projects/2026" (`create Projects/2026`, created after
`y`, then `undo`).

Other checks: `cargo clippy -p xtask --all-targets`: 0 warnings. `cargo
test -p zoo-infer -p zoo-abi -p xtask --locked`: all passed. Not run: a
clean-copy verify.

## POSIX step 4 results: writable files (version 0.13.0)

Design: docs/posix4-design.md. Calls 41–45 and the WRITE right (ABI
version 8), page-backed file contents, and the test program
SquirrelHandler, which Zookeeper runs during the directory checks (pid 8;
every later process id moved up by one).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 534 s,
`ZOO_OS BOOT version=0.13.0`; every case as expected (normal 5.0 s,
desktop 13.9 s, assistant 23.0 s, assistant-model 27.4 s, inference
398.4 s; `assistant-live` `SKIP`). Then `--accel whpx --case
assistant-live`: pass in 59.6 s.

From the normal case's serial log:

    ZOO_OS PROC start pid=8 name=squirrel-handler
    [8:squirrel-handler] SquirrelHandler: all file checks passed
    ZOO_OS PROC exit pid=8 name=squirrel-handler code=0
    ...
    ZOO_OS INFO processes done frames_free=520916 file_pages=1
    ZOO_OS TEST process-cleanup ok

`file_pages=1` is `Documents/notes.txt`, page-backed after SquirrelHandler
wrote to it and restored its contents; the OtterHandler and Assistant
searches that rely on its text still pass.

**A gap found and closed:** counting file pages as free in the leak check
would have hidden a file-removal leak. The check now also audits that the
file-page counter equals the pages files hold.

**Negative controls** (normal case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| `file_remove` does not free the file's pages | `FAIL normal`: `PANIC self-test process-cleanup: files hold 1 pages but 27 are counted` |
| Shrinking does not zero the rest of the last page | `FAIL normal`: `squirrel-handler code=43`, `check 43 failed: regrown tail is zero` |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings (one method renamed for a naming lint). `cargo
test -p zoo-infer -p zoo-abi -p xtask --locked`: all passed. Not run: a
clean-copy verify.

## POSIX step 3 results: exceptions (version 0.12.0)

Design: docs/posix3-design.md. Calls 38–40 (ABI version 7),
`zoo_rt::exception`, and the test program GeckoHandler, run by Zookeeper
at boot (pid 15, children 16–18; desktop and model cases' process ids
moved up by 4).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 626 s,
`ZOO_OS BOOT version=0.12.0`; every case as expected (normal 5.5 s,
desktop 14.1 s, assistant 23.2 s, assistant-model 31.7 s, inference
473.6 s; `assistant-live` `SKIP`). Then `--accel whpx --case
assistant-live`: pass in 60.4 s.

The normal case requires, in order:

    ZOO_OS PROC start pid=15 name=gecko-handler
    ZOO_OS PROC start pid=16 name=gecko-handler
    ZOO_OS PROC crash pid=16 name=gecko-handler fault=page-fault access=write cause=not-present addr=0x0000000000000010
    ZOO_OS PROC start pid=17 name=gecko-handler
    ZOO_OS PROC crash pid=17 name=gecko-handler fault=page-fault access=read cause=not-present addr=0x0000000000000020
    ZOO_OS PROC start pid=18 name=gecko-handler
    ZOO_OS PROC exit pid=18 name=gecko-handler code=103
    ZOO_OS PROC exit pid=15 name=gecko-handler code=0

Pid 16's fault was declared unhandled; pid 17's crash is the fault raised
inside its handler (address 0x20, not the original 0x10); pid 18's handler
returned a kernel-half `rip`, which the kernel refused. The kernel logged 33
deliveries to pid 15: 16 demand-commit faults on the main thread, one
divide error, and 16 on the second thread. GeckoHandler logs
`all exception checks passed`; `TEST process-cleanup ok` holds.

**Negative controls** (normal case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| `exception_return` does not reload the FPU/SSE state | `FAIL normal`: `gecko-handler code=22`, `check 22 failed: SSE register restored` |
| No guard against faults inside a handler | first try: **passed** (the re-delivered contexts stacked downwards until the thread's stack ran out, which also ended in a crash). The child now uses a fixed exception stack; rerun: `FAIL normal … got=timeout 30.0s`, 10,583 deliveries to pid 17 |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed. Not run: a clean-copy verify.

## POSIX step 2 results: address space (version 0.11.0)

Design: docs/posix2-design.md. Calls 32–37 (ABI version 6), `zoo_rt::vm`,
and the test program MoleHandler, which Zookeeper runs at boot (pid 10,
with four child processes 11–14 that must crash; desktop and model cases'
process ids moved up by 5).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 588 s,
`ZOO_OS BOOT version=0.11.0`; every case as expected (normal 5.4 s,
desktop 13.9 s, assistant 23.5 s, assistant-model 31.3 s, inference
440.5 s; `assistant-live` `SKIP`). Then `--accel whpx --case
assistant-live`: pass in 61.3 s.

The normal case requires these kernel lines in order (the crash lines are
matched exactly, including access type and cause):

    ZOO_OS PROC start pid=10 name=mole-handler
    ZOO_OS PROC start pid=11 name=mole-handler
    ZOO_OS PROC crash pid=11 name=mole-handler fault=page-fault access=read cause=not-present addr=0x0000000040000000
    ZOO_OS PROC start pid=12 name=mole-handler
    ZOO_OS PROC crash pid=12 name=mole-handler fault=page-fault access=write cause=protection addr=0x0000000040000000
    ZOO_OS PROC start pid=13 name=mole-handler
    ZOO_OS PROC crash pid=13 name=mole-handler fault=page-fault access=execute cause=protection addr=0x0000000040000000
    ZOO_OS PROC start pid=14 name=mole-handler
    ZOO_OS PROC crash pid=14 name=mole-handler fault=page-fault access=write cause=not-present addr=0x0000000040000000
    ZOO_OS PROC exit pid=10 name=mole-handler code=0

and MoleHandler logs `all address-space checks passed` (its in-process
checks are listed in the design, §5). `TEST process-cleanup ok` holds.

**Negative controls** (normal case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| `vm_commit`/`vm_protect` never set the execute permission | `FAIL normal`: `PROC crash pid=10 name=mole-handler fault=page-fault access=execute cause=protection addr=0x0000000040200000` (the generated-code check) |
| `vm_decommit`/`vm_release` free no pages | `FAIL normal`: `mole-handler code=16`, `check 16 failed: decommitted pages reserved again` |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings after replacing six `% PAGE != 0` with
`is_multiple_of`. `cargo test -p zoo-infer -p zoo-abi -p xtask --locked`:
all passed. Not run: a clean-copy verify.

## POSIX step 1 results: threads (version 0.10.0)

Design: docs/posix1-design.md. Process/thread split in the kernel, calls
27–31 (ABI version 5), `zoo_rt::thread`, and the test program AntHandler,
which Zookeeper runs at every boot (pid 9; desktop and model cases' process
ids moved up by one).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 501 s,
`ZOO_OS BOOT version=0.10.0`; every case as expected (normal 5.3 s,
desktop 14.0 s, assistant 23.2 s, assistant-model 27.5 s, inference
368.8 s; `assistant-live` reported `SKIP` without an accelerator). Then
`--accel whpx --case assistant-live`: pass in 49.1 s.

From the normal case's serial log:

    ZOO_OS PROC start pid=9 name=ant-handler
    ZOO_OS INFO thread start pid=9 tid=65536
    ...
    [9:ant-handler] AntHandler: 4 threads x 20000 rounds: counter 80000, lock waited 3 times, 4 handovers, TLS errors 0, main TLS 100, 34 ticks
    [9:ant-handler] AntHandler: all thread checks passed; exiting with a thread still running
    ZOO_OS PROC exit pid=9 name=ant-handler code=0
    ZOO_OS TEST process-cleanup ok

The last two lines show that a process exiting with a thread still running
ends cleanly with no leaked frames.

**Negative controls** (normal case; each temporary edit reverted):

| Change | Result |
| --- | --- |
| Kernel does not load the thread's FS base on a switch | `FAIL normal … ant-handler code=6`; `TLS errors 123, main TLS 3` |
| `futex_wake` wakes nobody | `FAIL normal … ant-handler code=10`; `futex_wake woke 0 (expected 1)` |

The first version of the futex check **passed** with `futex_wake` disabled:
the kernel's general wake-ups restart blocked calls, and the waiters then
saw the lock word changed. The check was replaced by one where only
`futex_wake` can release the waiter (value never changes, exactly one waiter
reported), and the kernel was fixed to count waiters that a general wake-up
had already made runnable.

Two findings while building the test:

- With a busy loop inside the locked region and no sleeping, the lock was
  never found taken under TCG (0 waits in two runs, although threads did
  interleave). The test now sleeps while holding the lock every 5,000
  rounds, so contention is deterministic; why timer preemption never landed
  inside the busy loop was not investigated further.
- Thread ids first came from the process-id counter, which shifted every
  later process id by the number of threads (the desktop and model cases
  failed). Extra threads now use ids from 65536.

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed. Not run: a clean-copy verify.

## M5c-2 results (version 0.9.0)

Design: docs/m5c2-design.md. New: ParrotHandler (the Assistant window),
ElephantHandler's service mode, guided tool choice (`infer/src/assist.rs`),
three verify cases and `cargo xtask assist-check`. The initrd holds 11
programs and 4 seed files (473,088 bytes).

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 485 s,
`ZOO_OS BOOT version=0.9.0`.

| Case | Result | Time | Notes |
| --- | --- | --- | --- |
| normal, panic, 3 fault modes, keyboard, scroll | OK | 2.7–6.7 s | unchanged |
| desktop | OK | 12.9 s | three windows now; process ids for the restarts moved (Parrot is one more app) |
| assistant | OK | 22.2 s | typed commands, plan + `y`, undo, not found, escape refused, "model is not installed" |
| assistant-model | OK | 25.3 s | see below |
| assistant-live | **SKIP** | — | `not run: needs a hardware accelerator (--accel whpx or kvm)` |
| inference | OK | 356.7 s | unchanged |
| 3 negative cases | rejected as intended | 1.8–5.0 s | unchanged |

Screen checks (exact text and colours, with a wrong string that must not
match):

    screen 1-plan: `Assistant` focused at (660, 416); search result at (660, 504); plan shown and confirmation asked at (660, 444); nothing created yet
    screen 2-transcript: created at (660, 544), undone at (660, 584), then not found; escape refused at (660, 684); missing model reported at (660, 724); wrong text rejected
    screen 1-busy: while the model works (status `Thinking:` at (660, 444)), Files took focus at (32, 52) and its selection moved to the 3rd entry at (32, 120)
    screen 2-cancelled: `Cancelled.` at (660, 504), ready again at (660, 444), no longer thinking

**Cancellation under TCG** (assistant-model serial log). The request was
cancelled while the system prompt was being computed; the service noticed
at its next token, 756 ticks (7.6 s) after Parrot sent `cancel`:

    [13:parrot-handler] ParrotHandler: cancel sent at tick 1038
    [14:elephant-handler] ElephantHandler: cancel seen at tick 1794, 1024 ticks into the request
    [14:elephant-handler] ElephantHandler: request cancelled

An earlier run of the same case gave 818 ticks. These are observations, not
performance claims.

**`--accel whpx --case assistant-live`** on the final code: exit 0 in 65 s
(case 47.0 s):

    [14:elephant-handler] ElephantHandler: system prompt 55 tokens in 1908 ticks
    [14:elephant-handler] ElephantHandler: `Find my pancake recipe`: margin 3, 10 request + 3 argument tokens, 2402 ticks
    [14:elephant-handler] ElephantHandler: chose search `pancake recipe`
    [14:elephant-handler] ElephantHandler: `Make a folder called Projects/2026`: margin 140, 15 request + 6 argument tokens, 762 ticks
    [14:elephant-handler] ElephantHandler: chose create `Projects/2026`
    screen 1-live: model chose `search pancake recipe` at (660, 504) (result shown) and `create Projects/2026` at (660, 564), created after `y` at (660, 644)

**`cargo xtask assist-check`** (host engine, same code): PASS, 8 of 8, in
39 s. The first request's tool margin is small (0.04 on the host, 3/100 in
the guest), so that choice is the least certain one.

| Request | Choice | Margin |
| --- | --- | --- |
| Find my pancake recipe | search `pancake recipe` | 0.04 |
| Search for files that mention flour | search `flour` | 2.72 |
| What is in my Documents folder? | list `Documents` | 1.53 |
| Show me what's inside Music | list `Music` | 0.29 |
| How big is Documents/notes.txt? | inspect `Documents/notes.txt` | 0.32 |
| Make a folder called Projects/2026 | create `Projects/2026` | 1.40 |
| Create a new folder named Invoices | create `Invoices` | 4.14 |
| Please create the folder Photos/Holidays | create `Photos/Holidays` | 5.37 |

**Negative controls:**

| Change | Result |
| --- | --- |
| ElephantHandler ignores `cancel` (temporary edit, reverted) | `FAIL assistant-model … got=timeout 300.0s`; the log shows `cancel sent` and no `request cancelled` |
| `assist-check` expecting `search "pancake"` (its first version) | `FAIL "Find my pancake recipe" -> search "pancake recipe" … expected search "pancake"`; exit 1. The expectation now accepts both, and OtterHandler's search matches all words against paths, so both find the recipe |

Other checks: `cargo clippy` (kernel, `zoo-user`, `xtask`, `zoo-abi`,
`zoo-infer`): 0 warnings. `cargo test -p zoo-infer -p zoo-abi -p xtask
--locked`: all passed (2 new tests for `assist`). Not run: a clean-copy
verify.

## M5c-1 results (version 0.8.0)

Design: docs/m5c1-design.md. In-memory filesystem (`kernel/src/fs.rs`),
directory capabilities and system calls 19–26 (ABI version 4), and the
action broker OtterHandler. The initrd now holds 10 programs and 4 seed
files (411,136 bytes); the kernel logs `ZOO_OS INFO fs seeded files=14
nodes=22`.

**Full `cargo xtask verify`** (TCG) in the main checkout: exit 0 in 442 s, all
12 cases as expected (normal 4.6 s, desktop 13.5 s, inference 356.0 s),
`ZOO_OS BOOT version=0.8.0`. The normal case now also requires
`PROC start pid=8 name=otter-handler` and its exit with code 0; the desktop
and inference cases' process ids moved up by 1. From the normal case's
serial log:

    [1:zookeeper] directory capabilities enforced (paths stay inside their handle)
    ZOO_OS PROC start pid=8 name=otter-handler
    [1:zookeeper] broker `search pancake` -> ok
    [1:zookeeper] broker `search FLOUR` -> ok
    [1:zookeeper] broker `list` -> ok
    [1:zookeeper] broker `inspect Documents` -> ok
    [1:zookeeper] broker `plan Projects/2026` -> ok
    [1:zookeeper] broker `inspect Projects` -> err not found
    [1:zookeeper] broker `create Projects/2026` -> ok
    [1:zookeeper] broker `list Projects` -> ok
    [1:zookeeper] broker `undo` -> ok
    [1:zookeeper] broker `inspect Projects` -> err not found
    [1:zookeeper] broker `undo` -> err nothing to undo
    [1:zookeeper] broker `create Documents` -> err already exists
    [1:zookeeper] broker `list ../bin` -> err path escapes home
    [1:zookeeper] broker `inspect /bin` -> err path escapes home
    [1:zookeeper] broker `delete Documents` -> err unknown tool
    [1:zookeeper] broker `create Projects/2027` -> ok
    [1:zookeeper] broker `undo` -> ok
    ZOO_OS PROC exit pid=8 name=otter-handler code=0
    [1:zookeeper] OtterHandler answered 17 scripted requests exactly

Each `->` line is logged only after every reply line of that request matched
the expected text exactly (`broker_step` in `user/src/bin/zookeeper.rs`).
`TEST process-cleanup ok` still holds.

**Negative controls** (normal case only, each reverted before the run above):

| Change | Result |
| --- | --- |
| Kernel path check accepts `..` (`valid_component` in `kernel/src/fs.rs`) | `FAIL normal … got ZOO_OS PROC exit pid=1 name=zookeeper code=26`; log: `dir_open ../bin: expected INVAL, got Err(NotFound)` |
| OtterHandler `undo` removes in creation order (parents first) | `FAIL normal … code=27`; log: ``broker `undo` reply 0: got `| not empty, left in place: Projects`, expected `| removed Projects/2026` `` |

Other checks: `cargo clippy` on the kernel, `zoo-user` (user target),
`xtask`, `zoo-abi` and `zoo-infer`: 0 warnings. `cargo test -p zoo-abi -p
xtask -p zoo-infer --locked`: all passed. Not run this step: a clean-copy
verify (the model image needs the 428 MB model copied in).

## M5b results (version 0.7.0)

Design: docs/m5b-design.md. New crate `zoo-infer` (GGUF reader, Q4_0/Q8_0,
Qwen2 forward pass, byte-level BPE tokenizer, ChatML); new program
ElephantHandler; new verify case `inference` using the model image. The
version number was not bumped: the kernel and the boot path are unchanged
from M5a.

Model: `tools/models/qwen2.5-0.5b-instruct-q4_0.gguf`, 428,730,208 bytes,
SHA-256 `7671c0c3…daf6ed` (full value and source in tools/models/README.md;
checked before every use).

**Independent reference** — `cargo xtask infer-check` (Rust engine vs
`tools/reference/qwen_ref.py`, NumPy 2.3.4, Python 3.12), exit 0 in 76 s:

| Prompt | Prompt tokens | Output tokens | Top-5 logit max diff |
| --- | --- | --- | --- |
| What is the capital of France? | 26 (same) | 7 (same) | 0.00006 |
| List three animals found in a zoo, one per line. | 31 (same) | 7 (same) | 0.00004 |
| Compute 12 + 30 and explain in one sentence. | 33 (same) | 11 (same) | 0.00007 |
| Translate 'café' to English; don't explain. | 31 (same) | 1 (same) | 0.00004 |
| Create a folder named Projects/2026 in my home directory. | 34 (same) | 24 (same) | 0.00005 |

"Same" means the token id sequences are identical; the threshold is 0.02.

**Full `cargo xtask verify`** on commit `0a96884` (TCG): exit 0 in 431 s, all
12 cases as expected (11 earlier cases unchanged, plus `inference` 350.5 s).
The host engine's answer for the guest prompt was
`The capital of France is Paris.` (ids 785, 6722, 315, 9625, 374, 12095, 13).
From the inference case's serial log:

    [8:elephant-handler] ElephantHandler: answer `The capital of France is Paris.`
    [8:elephant-handler] ElephantHandler: prefill 26 tokens in 26390 ticks, generated 7 tokens in 7632 ticks (100 ticks = 1 s)
    [8:elephant-handler] ElephantHandler: tokens match the host engine exactly
    ZOO_OS PROC exit pid=8 name=elephant-handler code=0

Timings are observations on this host, not performance claims:

| Where | Prompt (26 tokens) | Generation (7 tokens) | Case |
| --- | --- | --- | --- |
| Guest, TCG (run above) | 263.9 s | 76.3 s | 350.5 s |
| Guest, TCG (earlier run, same code) | 394.4 s | 104.2 s | 510.8 s |
| Guest, `--accel whpx --case inference` | 9.0 s | 2.4 s | 18.3 s |
| Host engine (release, one thread) | ≈ 5.9 tokens/s overall | | |

**Negative checks of the engine:** every truncation of a synthetic GGUF file
is rejected without a panic (unit test). ElephantHandler compares its ids with
`test/elephant.txt` and exits non-zero on a difference, and only exit 0
satisfies the marker; a deliberately wrong expectation was **not** run as a
separate case in this step. `--skip-model` reports the case as `SKIP`, never
`OK`.

## M5a results (version 0.7.0)

Design: docs/m5a-design.md. User programs now use SSE (custom target
`user/x86_64-zoo.json`, built with `-Zbuild-std`), have a growing heap
(`mem_alloc`) and can map initrd files read-only (`fs_map`). The kernel saves
and restores each user task's x87/SSE state on switches. Guest RAM: 2 GiB.

`cargo xtask verify` from a clean copy of the tracked files (path with spaces
and a comma, empty `target/` and `build/`, so `core`/`alloc` were rebuilt for
the new user target): exit 0 in 141 s, all 11 cases as expected (normal
4.8 s, desktop 13.5 s), `ZOO_OS BOOT version=0.7.0`. An earlier run in the
main checkout gave the same outcomes. The normal case now also requires
`PROC start pid=6/7 name=beaver-handler` and both exits (in any order); the
desktop case's process ids moved up by 2.

From the normal case's serial log:

    [1:zookeeper] mapped bin/zookeeper: 33480 bytes, ELF magic ok
    ZOO_OS PROC start pid=6 name=beaver-handler
    ZOO_OS PROC start pid=7 name=beaver-handler
    ZOO_OS PROC exit pid=6 name=beaver-handler code=0
    ZOO_OS PROC exit pid=7 name=beaver-handler code=0
    [1:zookeeper] both BeaverHandlers matched the reference bits (0x40e4abd67fe8c90c, 0x410b85587192e697)

- **FPU state is per process:** the two BeaverHandlers ran the same 3-million-
  step floating-point series at the same time (preempted many times) with
  different parameters, and each result equals the reference Zookeeper
  computed alone, bit for bit.
- **Heap:** each BeaverHandler fills a 4 MiB `Vec`, which needs the
  allocator to grow past its first 1 MiB with `mem_alloc`.
- **File mapping:** `fs_map` returns exactly the file's bytes (length equals
  `fs_stat`, ELF magic present).
- **No leaks:** `TEST process-cleanup ok` still holds with heap pages and
  ramdisk mappings in play.
- **SSE really used:** `llvm-objdump` of `beaver-handler` shows 22
  `mulsd`/`divsd`/`addsd`/`cvtsi2sd` instructions and no soft-float helper
  calls.

**Negative control (FPU switching).** With `fpu::save` and `fpu::restore` turned into no-ops, the normal image was booted headless: both BeaverHandlers exited 0, but Zookeeper logged `BeaverHandler 0 result 0x40e4abd67fe8c960, expected 0x40e4abd67fe8c90c` and exited with code 24 instead of 0, so the verifier's `PROC exit pid=1 name=zookeeper code=0` marker cannot match. Reverted before the runs above.

**WHPX:** the same normal image boots to READY with `-accel whpx` on the
development host; the timer self-test spun about 13× more than under TCG.
Verify does not use it.

## M4 results (version 0.6.0)

Design: docs/m4-design.md. A user-space window server (PeacockHandler), two
applications (OwlHandler: Files; ChimpHandler: crash test) and Zookeeper's
session mode, started with the shell command `desktop`.

Clean copy of the tracked files at `…\OriginsOS\build\clean checkout, spaces\`, empty
`target/` and `build/`, at version 0.6.0:

    cargo xtask doctor     -> ready, exit 0
    cargo xtask verify     -> exit 0 (70 s including builds)

| Case | Expected | Got | Time |
| --- | --- | --- | --- |
| normal | pass | pass | 3.1 s |
| panic | pass | pass | 3.1 s |
| fault-write-protect | pass | pass | 2.9 s |
| fault-heap-guard | pass | pass | 2.9 s |
| fault-stack-overflow | pass | pass | 2.8 s |
| keyboard | pass | pass | 5.0 s |
| scroll | pass | pass | 5.6 s |
| desktop (new) | pass | pass | 11.9 s |
| negative-wrong-marker | unexpected-marker | unexpected-marker | 1.9 s |
| negative-timeout | timeout | timeout | 5.0 s |
| negative-triple-fault | early-exit | early-exit | 2.0 s |

Afterwards: tracked files unchanged, firmware templates matched their
recorded hashes, no QEMU running.

The `desktop` case types into the guest through QMP in five phases, checks
the kernel markers between them (including two unordered groups where a
window server's apps exit while its replacement starts), and checks five
screenshots. Each screenshot check finds exact text by colour and also
requires a deliberately wrong string to be absent:

    1-desktop:        menu `ZooOS` at (8, 4), focused title `Files` at (32, 52), selected
                      `bin/zookeeper` at (32, 80), Chimp window shown; wrong text rejected
    2-details:        details of the 3rd file: `Name: bin/monkey-handler` at (32, 80),
                      `Size:` at (32, 100); wrong file rejected
    3-app-restart:    status `ChimpHandler crashed; restarted (1 of 3)` at (8, 780),
                      focused `Chimp` at (660, 52), Files details unchanged
    4-server-restart: fresh desktop after the window-server crash: `ZooOS` at (8, 4),
                      `Files` focused at (32, 52), list reset at (32, 80)
    5-console:        kernel console back: `Desktop session ended.` row 5,
                      `zoo> echo back` row 6, `back` row 7; no desktop left

Against the M4 acceptance criteria:

- **Keyboard navigation:** `Down`, `Enter`, `Tab` and `Esc` drive the
  desktop; screenshots 1–3 show the selection, the details view and the
  focus change.
- **Basic file browsing:** Files lists all seven programs in the initial
  filesystem with sizes, and opens the third one's details.
- **Crash recovery:** ChimpHandler's fault ends only ChimpHandler; the window
  server restarts it and Files keeps its state (screenshot 3). The window
  server's own fault (test key F12) releases the display; Zookeeper restarts
  it and a fresh desktop appears (screenshot 4).
- **Stable redraw:** frames are drawn into a back buffer and copied whole.
  All five screenshots were taken mid-session and every checked string was
  complete. This shows complete frames at those moments; it is not a frame-
  timing measurement.
- **Not a static screenshot:** the case is interactive end to end, and leaving
  the desktop returns a working kernel shell.

**Negative control (app restart).** With PeacockHandler's restart limit set to
0, the `desktop` case failed: `timeout: no ZOO_OS PROC start pid=10
name=chimp-handler within 90s`. Reverted before the runs above.

**Bug found by the first run.** The first full run failed only on screenshot
5: the shell printed its prompt before Zookeeper's last lines, so `echo back`
landed on a line of its own. Fixed (docs/m4-design.md §10); the next run
passed.

### M4 additional checks

| Check | Result |
| --- | --- |
| `cargo test -p xtask --locked` | 21 passed (adds unordered marker groups and coloured text search) |
| `cargo test -p zoo-abi` | 8 passed |
| `cargo clippy`: kernel in all 6 modes, `zoo-user`, `zoo-abi`, xtask | 0 warnings |
| `cargo fmt --all --check` | clean |

Not covered by automated checks: typing printable characters into an app
(no app uses them yet), the session's restart limit for the window server
itself (only one restart is exercised), and the `uptime` clock text.

## M3 results (version 0.5.0)

Design: docs/m3-design.md. User processes in separate address spaces,
`int 0x80` system calls, channels with handles, an ustar initrd, and the
Zookeeper scenario run at boot.

Clean copy of the tracked files at `…\OriginsOS\build\clean checkout, spaces\`, empty
`target/` and `build/` (so the user programs and initrd were built from
scratch):

    cargo xtask doctor     -> ready, exit 0
    cargo xtask verify     -> exit 0 (67 s including builds)

| Case | Expected | Got | Time |
| --- | --- | --- | --- |
| normal | pass | pass | 3.8 s |
| panic | pass | pass | 3.4 s |
| fault-write-protect | pass | pass | 3.4 s |
| fault-heap-guard | pass | pass | 3.4 s |
| fault-stack-overflow | pass | pass | 3.4 s |
| keyboard | pass | pass | 4.2 s |
| scroll | pass | pass | 5.0 s |
| negative-wrong-marker | unexpected-marker | unexpected-marker | 2.5 s |
| negative-timeout | timeout | timeout | 5.0 s |
| negative-triple-fault | early-exit | early-exit | 2.4 s |

Afterwards: tracked files unchanged, firmware templates matched their
recorded hashes, no QEMU running. That run was made just before the version
bump (its kernel printed `version=0.4.0`); `cargo xtask verify` was then
rerun in the main checkout on the 0.5.0 build (`ZOO_OS BOOT version=0.5.0`)
with the same ten outcomes.

The normal case now requires, in order, the six self-tests (including the
new `TEST address-spaces ok`), then these kernel markers, then READY:

    ZOO_OS PROC start pid=1 name=zookeeper
    ZOO_OS PROC start pid=2 name=lion-handler
    ZOO_OS PROC start pid=3 name=monkey-handler
    ZOO_OS PROC crash pid=3 name=monkey-handler fault=page-fault access=read cause=protection addr=0xffffc00000001000
    ZOO_OS PROC start pid=4 name=monkey-handler
    ZOO_OS PROC crash pid=4 name=monkey-handler fault=page-fault access=read cause=protection addr=0xffffc00000001000
    ZOO_OS PROC start pid=5 name=hamster-handler
    ZOO_OS PROC killed pid=5 name=hamster-handler by=1
    ZOO_OS PROC exit pid=2 name=lion-handler code=0
    ZOO_OS PROC exit pid=1 name=zookeeper code=0
    ZOO_OS TEST process-cleanup ok

User-process log lines from the same run (not markers; printed as
`[pid:name]`):

    [1:zookeeper] sent `ping 1`, got `pong: ping 1`
    [3:monkey-handler] ZOO-OS PANIC forged marker from user space
    [1:zookeeper] MonkeyHandler crashed (run 1 of 2)
    [1:zookeeper] restart policy allows it: restarting MonkeyHandler
    [1:zookeeper] restart limit reached: giving up on MonkeyHandler
    [5:hamster-handler] HamsterHandler: running on the wheel, never yielding
    [1:zookeeper] sent `ping 2`, got `pong: ping 2`
    [1:zookeeper] HamsterHandler stopped by the supervisor
    [1:zookeeper] Zookeeper: all checks passed

What these show, against the M3 acceptance criteria:

- **Two isolated processes communicate:** Zookeeper and LionHandler exchange
  `ping`/`pong` over a channel. `TEST address-spaces ok` shows two spaces map
  the same user address to different frames and share only the
  supervisor-only kernel half.
- **Invalid handles and malformed requests fail safely:** MonkeyHandler makes
  14 bad requests (unknown syscall, bogus/stale handles, kernel and null
  pointers, oversized length, unknown program, bad exit code, closed peer).
  Each must return the exact error or it exits with a code instead of
  crashing, which would change the markers. Its forged `ZOO_OS PANIC` text
  came out as `ZOO-OS PANIC`, so user text cannot create markers.
- **A crashing process does not affect others:** MonkeyHandler's read of
  kernel memory (`error=0x5`: present page, user mode) ends only that
  process, twice; LionHandler answers `ping 2` afterwards.
- **Supervisor with explicit failure behaviour:** one restart, then give up;
  `kill` for a process that never yields.
- **Preemption:** `ping 2` completes while HamsterHandler spins.
- **No leaks:** the free-frame count after all five processes were reaped
  equals the count before Zookeeper started.

**Negative control (preemption):** with the timer handler changed to never
switch away from a user task, the normal image was booted headless for 45 s.
Output stopped right after
`[5:hamster-handler] HamsterHandler: running on the wheel, never yielding`;
no `ping 2`, no further markers, QEMU killed by the timeout. The verifier's
30 s timeout would reject it. The change was reverted before the runs above.

### M3 additional checks

| Check | Result |
| --- | --- |
| `cargo test -p zoo-abi` | 8 passed: ELF and tar parsers on valid input, every malformed-header and bad-segment case, and every truncation of a valid file (no panics) |
| `cargo test -p xtask --locked` | 19 passed |
| `cargo clippy`: kernel in all 6 modes, `zoo-user`, `zoo-abi`, xtask | 0 warnings |
| `cargo fmt --all --check` | clean |
| Regression after the trap-path and layout changes alone, before any process code | all 10 cases passed; physical window at `0xffff818000000000`, boot stack at `0xffff808000001000` |

Not covered by automated checks: the shell's `ps`, `ls` and `run` commands.

## Rename results (version 0.4.0)

The rename changed names only: crates (`zoo-os-kernel`), images
(`zoo-os-<mode>-uefi.img`), serial markers (`ZOO_OS …`), the shell prompt
(`zoo> `) and docs. No kernel logic changed.

Clean copy of the tracked files at `…\OriginsOS\build\clean checkout, spaces\`,
empty `target/` and `build/`:

    cargo xtask doctor     -> "ZooOS doctor", ready, exit 0
    cargo xtask verify     -> exit 0 (52 s including builds)

| Case | Expected | Got | Time |
| --- | --- | --- | --- |
| normal | pass | pass | 3.1 s |
| panic | pass | pass | 3.0 s |
| fault-write-protect | pass | pass | 2.9 s |
| fault-heap-guard | pass | pass | 2.9 s |
| fault-stack-overflow | pass | pass | 2.9 s |
| keyboard | pass | pass | 3.5 s |
| scroll | pass | pass | 4.2 s |
| negative-wrong-marker | unexpected-marker | unexpected-marker | 1.8 s |
| negative-timeout | timeout | timeout | 5.0 s |
| negative-triple-fault | early-exit | early-exit | 1.9 s |

Afterwards: tracked files unchanged, firmware templates matched their
recorded hashes, no QEMU running. A run in the main checkout gave the same
outcomes, with these screen checks:

    keyboard: 1280x800, 19241 lit pixels; row 0 = BOOT line, row 15 = `zoo> echo hi`,
              row 16 = `hi`; wrong version rejected
    scroll:   43 rows; rows 40-42 = `zoo> echo end`, `end`, prompt; BOOT scrolled off

Kernel output in the keyboard case begins `ZOO_OS BOOT version=0.4.0`.
`cargo test -p xtask --locked`: 19 passed. `cargo clippy`: 0 warnings for
the kernel in all six modes and for xtask. `cargo fmt --all --check`: clean.

## Interim console results (version 0.3.0)

Design: docs/console-design.md. Normal boots now start a debug shell on the
framebuffer and PS/2 keyboard after `READY`, and print `ORIGINS_OS SHELL
ready`. Verify cases that type keys add `-qmp tcp:127.0.0.1:<free
port>,server=on,wait=off` to the QEMU command line and use QMP `send-key`
and `screendump`.

Clean copy of the tracked and new files at
`…\OriginsOS\build\clean checkout, spaces\`, empty `target/` and `build/`:

    cargo xtask doctor     -> ready, exit 0
    cargo xtask verify     -> exit 0 (55 s including builds)

| Case | Expected | Got | Time |
| --- | --- | --- | --- |
| normal | pass | pass | 3.1 s |
| panic | pass | pass | 3.1 s |
| fault-write-protect | pass | pass | 3.0 s |
| fault-heap-guard | pass | pass | 3.0 s |
| fault-stack-overflow | pass | pass | 3.0 s |
| keyboard | pass | pass | 3.7 s |
| scroll | pass | pass | 4.3 s |
| negative-wrong-marker | unexpected-marker | unexpected-marker | 2.0 s |
| negative-timeout | timeout | timeout | 5.0 s |
| negative-triple-fault | early-exit | early-exit | 2.1 s |

Afterwards: tracked files unchanged, firmware templates matched their
recorded hashes, no QEMU running. Two earlier runs in the main checkout gave
the same outcomes (the first before the `scroll` case existed).

Screen checks reported by the run in the main checkout:

    keyboard: 1280x800, 21362 lit pixels; row 0 = BOOT line, row 15 = `origins> echo hi`,
              row 16 = `hi`; wrong version rejected
    scroll:   43 rows; rows 40-42 = `origins> echo end`, `end`, prompt; BOOT scrolled off

"Row N = text" means every pixel of those glyph cells equals the text
re-rendered on the host with the same font and layout. The deliberately
wrong string `ORIGINS_OS BOOT version=9.9.9` is checked against row 0 in
the same run and does not match.

Serial log of the keyboard case after the shell starts (`^H` is the
backspace echo):

    ORIGINS_OS SHELL ready
    Type 'help' for commands.
    origins> echx^H ^Ho hi
    ORIGINS_OS SHELL line=echo hi
    hi
    origins> mem
    ORIGINS_OS SHELL line=mem
    frames: 127911 free of 128172 (511644 KiB free)
    heap:   16 bytes used of 1048576
    origins>

The 16 heap bytes are the shell's line buffer, which keeps its capacity
after `clear()`.

### Console additional checks

| Check | Result |
| --- | --- |
| `cargo test -p xtask --locked` | 19 passed (adds screenshot render/compare and PPM parsing tests) |
| `cargo clippy` on the kernel in all 6 modes, and on xtask | 0 warnings |
| `cargo fmt --all --check` | clean |
| `cargo xtask run` | not re-run with the window in this record; it now opens a QEMU window instead of running headless |

Not covered by automated checks: what the panic and fault kernels draw on
screen (their serial output is checked), `clear`, `uptime`, `keys`,
`panic` and `reboot` typed at the prompt.

## M2 results (version 0.2.0)

Design: docs/m2-design.md. Six kernel builds (normal, panic and four test
modes), each with its own Cargo feature, target directory and image.

Clean copy of the tracked and new files at
`…\OriginsOS\build\clean checkout, spaces\`, empty `target/` and `build/`:

    cargo xtask doctor     -> ready, exit 0
    cargo xtask verify     -> exit 0   (run 1, 40 s including builds)
    cargo xtask verify     -> exit 0   (run 2, 24 s)

Both runs, identical outcomes:

| Case | Expected | Got | Time (run 1 / 2) |
| --- | --- | --- | --- |
| normal | pass | pass | 2.9 s / 2.9 s |
| panic | pass | pass | 2.9 s / 2.9 s |
| fault-write-protect | pass | pass | 2.8 s / 2.8 s |
| fault-heap-guard | pass | pass | 2.7 s / 2.8 s |
| fault-stack-overflow | pass | pass | 2.9 s / 2.9 s |
| negative-wrong-marker | unexpected-marker | unexpected-marker | 1.8 s / 1.8 s |
| negative-timeout | timeout | timeout | 5.0 s / 5.0 s |
| negative-triple-fault | early-exit | early-exit (QEMU exited, exit code 0) | 1.9 s / 1.9 s |

Afterwards: tracked files unchanged, firmware templates matched their
recorded hashes, no `qemu-system-x86_64.exe` running. An earlier run in the
main checkout gave the same eight outcomes.

Kernel output from the normal case:

    ORIGINS_OS BOOT version=0.2.0
    ORIGINS_OS INFO mode=normal memory_regions=105 usable_bytes=525803520
    ORIGINS_OS INFO memory phys_offset=0x28000000000 stack_bottom=0x18000001000 stack_len=0x20000 heap=0xffffc00000001000+0x100000
    ORIGINS_OS TEST breakpoint ok
    ORIGINS_OS INFO frames total=128202 free=127943
    ORIGINS_OS TEST frames ok
    ORIGINS_OS TEST paging ok
    ORIGINS_OS INFO heap size=1048576 churned_bytes=12582912 used=0
    ORIGINS_OS TEST heap ok
    ORIGINS_OS INFO timer hz=100 ticks=10 spins=534033
    ORIGINS_OS TEST timer ok
    ORIGINS_OS READY

Fault cases (marker line, then details):

    fault-write-protect:
    ORIGINS_OS EXCEPTION page-fault access=write cause=protection addr=0xffffc00040000000
    ORIGINS_OS INFO exception rip=0x0000010000003b10 rsp=0x0000018000020ea0 error=0x3

    fault-heap-guard:
    ORIGINS_OS EXCEPTION page-fault access=read cause=not-present addr=0xffffc00000101000
    ORIGINS_OS INFO exception rip=0x00000100000037fe rsp=0x0000018000020ed0 error=0x0

    fault-stack-overflow:
    ORIGINS_OS EXCEPTION double-fault stack-guard-hit
    ORIGINS_OS INFO double_fault cr2=0x0000018000000f48 stack_bottom=0x0000018000001000

    negative-triple-fault: output stops after the INFO memory line; QEMU exits.

What these show:

- The bootloader kept all its mappings in the lower half (kernel at
  0x100_0000_0000, stack at 0x180_0000_1000, physical window at
  0x280_0000_0000), so the kernel-owned upper half was empty as required.
- 259 frames are in use after the self-tests: 256 heap pages plus 3
  page-table frames created for the upper-half mappings. Test allocations were
  all returned.
- The overflow's faulting address 0x18000000f48 lies in the guard page
  [0x18000000000, 0x18000001000) directly below the stack.
- Error codes match the cause: 0x3 = present + write; 0x0 = not present, read.
- No case rebooted: fault kernels stay halted until the harness stops QEMU,
  and the deliberate triple fault is reported as `early-exit`.

### M2 additional checks

| Check | Result |
| --- | --- |
| `cargo test -p xtask --locked` | 16 passed (adds exact fault-marker and self-test-panic cases) |
| `cargo clippy` on the kernel in all six modes, and on xtask | no warnings |
| Kernel built with two test features at once | rejected: `at most one test-mode feature may be enabled` |
| `cargo fmt --all --check` | clean |

## What the harness checks

- COM1 output is written to `serial.log` by QEMU itself, so only guest output
  can satisfy it. The harness polls that file every 50 ms.
- Every line containing `ZOO_OS ` is a marker (except `INFO` lines without
  `mode=`). Markers must match the expected sequence exactly and in order; any
  other marker fails immediately. Every case starts with
  `BOOT version=<crate version>` and `INFO mode=<mode> …`. M2 tails:
  normal: five `TEST <name> ok` lines, then `READY`; panic: the same five,
  then `PANIC intentional-test`; fault modes: one exact `EXCEPTION …` line
  (listed in docs/m2-design.md §9). A self-test failure prints `PANIC
  self-test …`, which never matches.
- After the last marker it watches for 1 s more (`--settle-ms`) so a trailing
  marker (e.g. READY after PANIC) still fails. A halted guest is then killed;
  that is a pass, not a timeout.
- Timeout (default 30 s, `--timeout-secs`), QEMU exiting by itself, or a wrong
  marker all fail. QEMU is always killed and reaped (bounded 10 s wait).
- The `INFO` line reports the number of memory regions and usable bytes read
  from the bootloader's `BootInfo` at runtime.

## M1 results (version 0.1.0)

### M0: doctor

| Command | Result |
| --- | --- |
| `cargo xtask doctor` | `ready: all prerequisites found`, exit 0 |
| `QEMU_X86_64=C:/nope/qemu.exe cargo xtask doctor` | 3 `[FAIL]` lines (QEMU, and both firmware files, which are found relative to QEMU), each with a fix; `not ready`, exit 1 |

### M1: boot verification

Run from a fresh copy of only the tracked files, at
`…\OriginsOS\build\clean checkout, spaces\` (a path with spaces and a comma),
with empty `target/` and `build/` directories:

    cargo xtask doctor     -> ready, exit 0
    cargo xtask verify     -> exit 0

    OK    normal                 expected=pass              got=pass                 2.7s  qemu terminated by harness
    OK    panic                  expected=pass              got=pass                 2.7s  qemu terminated by harness
    OK    negative-wrong-marker  expected=unexpected-marker got=unexpected-marker    1.8s  qemu terminated by harness
          detail: got `ORIGINS_OS INFO mode=panic-test memory_regions=105 usable_bytes=530128896` while expecting `ORIGINS_OS INFO mode=normal …`
    OK    negative-timeout       expected=timeout           got=timeout              5.0s  qemu terminated by harness
          detail: no `ORIGINS_OS NEVER-EMITTED` within 5s
    [verify] PASS: 2 positive cases passed, 2 negative cases were rejected as intended

A second `verify` in the same copy also exited 0. Afterwards: SHA-256 of
every tracked file was unchanged, both firmware templates still matched the
recorded hashes, and `tasklist` showed no `qemu-system-x86_64.exe` running.

Tail of the normal serial log (bootloader lines, then kernel output):

    INFO : Jumping to kernel entry point at VirtAddr(0x10000001bc0)
    ORIGINS_OS BOOT version=0.1.0
    ORIGINS_OS INFO mode=normal memory_regions=104 usable_bytes=530132992
    ORIGINS_OS READY

Tail of the panic serial log:

    INFO : Jumping to kernel entry point at VirtAddr(0x10000001cd0)
    ORIGINS_OS BOOT version=0.1.0
    ORIGINS_OS INFO mode=panic-test memory_regions=105 usable_bytes=530128896
    ORIGINS_OS PANIC intentional-test
    ORIGINS_OS INFO panic_location=kernel\src\main.rs:40

### Additional checks

| Check | Result |
| --- | --- |
| `cargo test -p xtask --locked` (marker logic) | 14 passed |
| `cargo clippy -p xtask`; `cargo clippy -p origins-os-kernel --target x86_64-unknown-none` (with and without `panic-test`) | no warnings |
| `cargo fmt --all --check` | clean |
| `cargo xtask run --timeout-secs 10` | COM1 markers printed on the terminal; QEMU stopped after 10 s; none left running |
| Early exit: `OVMF_CODE=<12-byte file> cargo xtask verify` | QEMU exited 1 (`pflash0 has invalid size`); all 4 cases `early-exit` in 0.2 s; verify exit 1 |

## Known limitations

- **Windows path length.** On this host `LongPathsEnabled` is 0. Cargo's
  build directory for this nightly is deeper than before, and a checkout whose
  root path is roughly 120+ characters makes MSVC `link.exe` fail with
  `LNK1104` on a 260-character path while building host dependencies
  (observed at exactly 260 characters). Use a shorter checkout path or set
  `CARGO_TARGET_DIR` to a short directory. This is a host limit; enabling
  long paths is a host configuration change and was not made.
- Only TCG (software emulation) is tested. WHPX/KVM acceleration is unused.
- Only the Windows host is tested. Linux firmware fallbacks are untested.
- `bootloader`'s build script runs `cargo install bootloader-x86_64-uefi
  --locked` from crates.io, so the first build needs network access.
- QEMU timings above are functional observations, not performance results.
  The timer self-test's `spins` count is not a performance measure.
- M2 timer uses the legacy 8259 PIC and PIT. It works on QEMU q35 with OVMF;
  real hardware will need the local APIC/IOAPIC (deferred).
- The double-fault IST stack is a static array with no guard page of its own.
- Out-of-memory in the heap is not exercised by a test case; by design it
  panics (Rust's default `no_std` allocation-failure handling).
- The console scrolls by copying video memory, which is fine in QEMU but slow
  on real hardware. Only the PS/2 keyboard is supported; most real PCs need
  USB input. QMP uses a free localhost port picked just before QEMU starts,
  which leaves a small race if another program takes it first.
