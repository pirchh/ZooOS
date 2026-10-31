# Alpine Linux root file system (Wine build environment)

Downloaded, never committed (ignored by Git). The owner chose to build Wine
from source on 2026-10-31 and enabled WSL themselves for it; this minimal
file system becomes the build environment with `wsl --import`, which only
creates a virtual disk under `build/` (no other Windows change).

| Field | Value |
| --- | --- |
| File | `alpine-minirootfs-3.24.3-x86_64.tar.gz`, 3,704,080 bytes |
| Source | https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/ (with `.sha256` and `.asc`) |
| SHA-256 | `f8e1cafba9b1b283b5d30d06f9885c8d2bc161ffe9083ade8e2e157813a53407` (equals Alpine's `latest-releases.yaml` and the `.sha256` file) |
| Signature | Good signature from "Natanael Copa <ncopa@alpinelinux.org>", key `0482 D840 22F5 2DF1 C4E7 CD43 293A CD09 07D9 495A` (from https://alpinelinux.org/keys/ncopa.asc, checked in a throwaway GnuPG home) |

Why Alpine: it is built on musl, like ZooOS's Linux layer, and packages
Wine for musl, so its build recipe is a tested starting point.
