# Services step 5 (S5): files that survive a reboot

Status: implemented and verified 2026-10-31 (results in docs/verification.md;
§5 records the differences from this plan). **Superseded by ZooFS 2 in L10c
(docs/linux10c-design.md, 2026-11-01)**, which keeps this document's two
slots, generations and recovery rules but stores file data block by block. Builds on S4b (the
filesystem service BadgerHandler). DECISIONS.md asks for recovery to be
documented before personal data is trusted to the disk; §3 is that.

## 1. Disk

- QEMU gets a second drive: a virtio block device (`-drive
  if=none,id=data,file=build/data.img` + `-device virtio-blk-pci`), 16 MiB,
  created by xtask. The boot image stays read-only in spirit; user data
  lives only on this drive. On real hardware this would be a different
  driver (NVMe/AHCI); that comes with the USB real-hardware milestone.
- **Kernel driver** (`kernel/src/virtio_blk.rs`): finds the device by PCI
  config-space scan (port I/O 0xCF8/0xCFC; vendor 0x1AF4, legacy device id
  0x1001), uses the legacy (I/O-port) virtio interface with one queue,
  **polled** (no interrupts), 512-byte sectors. Small and verifiable first;
  interrupts and the modern interface later.
- **Capability:** `block_attach` (new call) returns a block-device handle
  only to the resident supervisor (like `log_attach`). `block_read` /
  `block_write` (sector, 4 KiB-aligned buffer) work only through that
  handle. Zookeeper passes it to BadgerHandler (`etc/services` flag
  `disk`). No other program can touch the disk.

## 2. On-disk format (in BadgerHandler)

- Two **slots**, A and B, each half the disk. A slot is a header sector
  (magic `ZOOFS001`, generation `u64`, payload length, CRC-32 of the
  payload) followed by the payload: a serialised tree of `/home/user`
  (folders, files, contents). `/bin` is never saved (it comes from the
  initrd).
- **Load at start:** read both headers; take the slot with the highest
  generation whose magic, length and CRC are valid; rebuild `/home/user`
  from it. No valid slot (fresh disk): seed from the initrd as today.
- **Save:** after a change (debounced: at most once per 50 ticks), write the
  payload to the slot that is *not* the current one, then its header with
  generation + 1. The header is written last.

## 3. Crash consistency (what survives what)

| Interruption | Result after reboot |
| --- | --- |
| Power lost while writing the payload | The new slot's CRC fails; the previous slot (older generation, intact) is loaded: changes since the last completed save are lost, nothing is corrupted. |
| Power lost while writing the header sector | Same as above (a torn header fails its magic/CRC). |
| Power lost after the header | The new snapshot is complete and loaded. |
| Changes made in the last ≤ 50 ticks before power loss | Lost (debounce). |

This assumes a sector write is atomic or detectably torn (checked by the
CRC), which holds for QEMU's virtio-blk and for the hardware we would
target; it is not a journal, and snapshots rewrite the whole tree (fine for
megabytes, not for large data).

## 4. Verification

- Two-boot case on one fresh data disk: boot 1 creates
  `Documents/kept.txt` with known text (through the shell or a test
  program) and waits for the save; boot 2 must list and read it back.
- Crash case: a test hook in BadgerHandler stops after writing a payload
  but before its header (simulating power loss); the next boot must load
  the previous generation intact.
- Corruption case: xtask flips bytes in the newest slot's payload between
  boots; the older slot must be loaded and the fallback logged.
- Negative controls: header written before the payload (the crash case must
  then fail), and loading without CRC checking (the corruption case must
  fail).

## 5. As implemented

- **Driver** `kernel/src/virtio_blk.rs` (legacy interface, QEMU started with
  `disable-modern=on`); the queue needs three physically contiguous pages,
  so the frame allocator gained `allocate_contiguous`. Calls 52
  `block_attach` (supervisor only), 53 `block_read`, 54 `block_write`
  (4 KiB blocks), 55 `block_size`; ABI version 14.
- **xtask:** every QEMU run gets a data disk (16 MiB). Verify cases get a
  fresh one; the `persist-*` cases share `build/disks/persist.img`;
  `cargo xtask run` keeps `build/data.img` between runs.
- **Service list:** `badger-handler 3 disk` (the flag passes the block
  handle as the second start-up handle).
- **Shell:** `dir [path]`, `cat <path>`, `put <path> <text>` work on the home
  folder through the supervisor and BadgerHandler.
- **Snapshot payload:** records `kind u8 | path len u16 | path | size u32 |
  data` for everything under `/home/user`, preorder. Header block: magic,
  generation, length, CRC-32.
- **Test hook:** `poke badger-handler half-save` makes the next save stop
  after its payload (exit code 41); the supervisor restarts the service.
- **Ordering versus CRC:** the plan listed "header written before the
  payload" as a negative control. It is not discriminating here: a header
  written first describes a payload that is not on the disk yet, so its CRC
  fails and the slot is rejected anyway. The CRC is what protects against
  torn saves; writing the header last additionally avoids ever presenting a
  complete-looking header for incomplete data. The controls used instead
  disable the CRC check and the newest-generation choice.
- **Cases:** `persist-1` (fresh disk; generations 1 and 2 saved to A and
  B), `persist-2` (reboot; generation 2 loaded; file content and size
  correct), `persist-3` (xtask damages generation 2; it is rejected and
  generation 1 loaded; then a simulated power loss mid-save; after the
  service restarts generation 1 still loads). They must run in this order
  (`--case persist-2` alone has no disk to read).

