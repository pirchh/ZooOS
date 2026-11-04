# Bella's models on the USB stick (2026-11-03)

Status: implemented and verified in QEMU. Not yet run on real hardware.
Part of the hardening before the first real-PC test (docs/usb-boot.md).

## 1. The problem

A real PC has no model disk: ZooOS reads only QEMU's virtio disks, and
has no USB storage driver yet. So the stick must carry Bella's models in
its boot image, which the bootloader loads into memory with the kernel.
Since Qwen3-1.7B became the default (0.98.0), the stick could not:
`usb-image` stopped at a 600 MB cap, and a stick built with `--bella 0.5b`
gave the smaller, less accurate model.

## 2. The cause of the cap, found

The cap was set at a size that worked, never measured. Raising it showed
the real limit: `bootloader` 0.11.17 sizes its FAT partition as the files'
total plus about 2 MiB (`fat.rs`, `create_fat_filesystem`). The two FAT
tables alone take about 0.2% of the volume, 3.5 MB at 1.7 GB, so the copy
fails:

```
failed to create FAT filesystem: failed to copy source data `... initrd-model.tar`
to file at `ramdisk`: No space left on device
```

## 3. The fix

- **`xtask/src/uefi_image.rs` builds the image.** `bootloader` only exports its
  files (`DiskImageBuilder::create_uefi_tftp_folder`: its UEFI loader,
  the kernel, the ramdisk, `boot.json`). xtask lays them out: a FAT
  partition with room for its tables (data + 1/128 + 8 MiB), inside a GPT
  disk with one EFI system partition, as `bootloader`'s own `gpt.rs`
  does. Every image now goes through it, so the whole suite checks it.
- **`fatfs` 0.3.6 and `gpt` 3.1.0,** the versions `bootloader` already
  uses: pinned in `xtask/Cargo.toml`, nothing new downloaded.
- **Both models in the stick's boot image** (`image::build_usb_image`):
  `models/chat.gguf` and `models/embed.gguf` (`zoo_abi::CHAT_MODEL_FILE`,
  `EMBED_MODEL_FILE`). ElephantHandler already mapped a chat model from
  the boot image. MagpieHandler now does the same before trying the model
  disk, and Bella starts it when either has an embedding model.

**`verify` still uses the model disk for the Bella cases** (the 600 MB
`BOOT_IMAGE_MODEL_MAX` now only chooses that). They test what `run`
boots, and changing all of them at once is not this step.

## 4. Verified

New case `usb-model` (`--accel`): boots the stick's image (fast boot,
1.8 GB, 4 GiB of memory, **no model disk**) and asks "Find my pancake
recipe".

| Check | Result |
| --- | --- |
| `usb-model`, 8 processors | `OK`: "the embedding model is in the boot image (609 MB)"; "service ready (model mapped, parsed in 437 ms)"; ``chose search `pancake` ``; answered in 1169 ms; no "from the model disk" |
| Negative control: MagpieHandler looking for a wrong file name | `FAIL`: ``ZOO_OS PROC exit pid=10 name=magpie-handler code=3`` |
| `bella-model` with Qwen3 in the normal boot image (1.13 GB) | `OK`: booted, model mapped |
| Same, with `bootloader`'s own builder (before) | build fails: "No space left on device" |
| `cargo xtask usb-image` | `build/usb/zoo-os-usb.img`, 1,796,276,224 bytes |

Mapping instead of reading also saves memory: ElephantHandler's heap was
302 MB here against 1.2 GB with the model disk, where the model is
copied into it.

## 5. Process exit, measured

The other kernel path left open by docs/keys-design.md: a process exiting
frees its pages under the scheduler lock. Measured in `model-restart` (8
processors, timing lines as in keys-design, not committed): killing the
1.2 GB model service never held the lock over 20 Mcycles. Freeing is
cheap; zeroing new pages was the slow part. Not changed.

## Not done

- **Real hardware:** the next step is the owner's test on their other PC
  (docs/usb-boot.md: at least a 2 GB stick and 4 GB of memory now).
- Files still do not persist on a real PC (no USB storage driver).
- The stick's image is rebuilt whole (1.8 GB) each time.
