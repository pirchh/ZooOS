# Services step 2 (S2): every program's logs through BeaverHandler

Status: implemented and verified 2026-10-30 (results in docs/verification.md).
Builds on S1 (docs/s1-design.md).

## 1. Path of a log line

1. A program calls `log` (system call 3), as before.
2. The kernel prints `[pid:name] text` on the serial console, as before
   (the verifier and humans watching QEMU rely on it), **and** appends
   `<tick> <pid> <name> <text>` to the kernel log buffer
   (`kernel/src/klog.rs`).
3. BeaverHandler drains the buffer through a **log-reader handle** and
   keeps the last 256 lines, formatted `[secs.cc] [pid:name] text`.
4. `logs [text]` in the shell: the kernel asks Zookeeper, Zookeeper asks
   BeaverHandler (`dump text`), and the matching lines (at most 30, with a
   note of how many earlier ones were skipped) come back.

## 2. The buffer and the capability

- 128 lines in the kernel heap; when full the oldest line is dropped and
  the next read starts with `N earlier lines dropped`. Lines logged during
  boot, before BeaverHandler runs, may therefore be partly dropped.
- `log_attach` (47) returns a log-reader handle, only to the resident
  supervisor (a per-process permission set by the kernel when it starts
  it). `log_read(handle, buf)` (48) returns one line or `AGAIN`. Reading
  removes the line, so there is one reader in practice.
- Zookeeper gives a fresh reader to services marked `logs` in
  `etc/services` (`beaver-handler 3 logs`) as their second start-up
  handle, on every (re)start. ABI version 10.
- Zookeeper's own supervision events now reach BeaverHandler the same way
  (its direct `log` messages from S1 were removed).

## 3. Verification

The `services` case additionally runs `puffin-test` and requires
`logs puffin` to show its last line in BeaverHandler's format
(`] [26:puffin-test] PuffinTest: all C checks passed`), which the
program's own console line cannot match. Negative control: the kernel not
copying lines into the buffer.

## 4. Not in S2

Log levels, per-service filtering rights, persistence, and services finding
each other by name (S3, which first needs handles passed in messages).
