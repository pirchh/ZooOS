# Assistant step 3 (A3): the model in the everyday system

Status: implemented 2026-11-02 (version 0.73.0). The owner asked whether
the model should be "baked into the actual os". It now is.
- **Before:** the everyday system (`cargo xtask run`, the fast-boot image,
  and the USB image) had no model, so a request in words got "the model
  is not installed". The model came only with `run --model`, which boots
  the test (normal) image.
- **Now:** both include the model whenever it is in `tools/models/`.

## 1. What changed (`xtask`)

- **`build_model_image`** takes the image mode. The everyday image with
  the model is `build/fast-boot-model/zoo-os-fast-boot-model-uefi.img`
  (initrd 411 MB).
- **`cargo xtask run`** (the fast-boot mode) builds and boots that image
  when the model is there, and says so. **`--no-model`** leaves it out.
  Without a model file it says so and boots as before.
- **`cargo xtask usb-image`** does the same. `--no-model` makes the small
  image, as before.
- **`run --model`** is unchanged: the normal image with the model, which
  is what `assistant-live` uses.

## 2. What it costs (measured)

Boot times under `--accel whpx`, from QEMU starting, in each run's
`serial-times.log` (milliseconds):

| | Kernel up (`BOOT`) | Desktop up (Assistant started) |
| --- | --- | --- |
| Everyday image without the model (`fast-boot`) | 918 | 1080 |
| Everyday image with the model (`everyday-model`) | 2753 | 2930 |

- **The model adds about 1.85 s to boot,** all of it before the kernel
  starts: the boot loader reads the 411 MB larger image.
- **The first request in words** then starts the model service, which
  maps the model and reads the system prompt in 3.44 s. The answer
  follows in about 1 s.
- **Requests answered from memory (A2), and typed commands,** do not wait
  for the model.

## 3. Check

- **`everyday-model`** (with an accelerator, because the model is too
  slow under TCG; skipped with `--skip-model` or TCG):
  - the everyday image with the model starts straight in the desktop;
  - `Find my pancake recipe` in the Assistant →
    ``ElephantHandler: chose search `pancake recipe` ``.

  `OK` with `--accel whpx`.

## 4. Next (not done)

- **Load the model after boot, from the data disk,** instead of in the
  boot image. That wins back the 1.85 s, and the image stays small for
  USB.
- **Warm the model up in the background** when the desktop starts (the
  3.4 s system prompt), so the first question is answered in about 1 s.
