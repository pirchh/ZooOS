# The kernel heap under pressure (B13)

Status: implemented in version 0.89.0. Audit item B13 (docs/bella-audit.md),
Stage 2 of docs/bella-roadmap.md.

## The problem

The kernel heap is a fixed 8 MiB (`kernel/src/memory/mod.rs`,
`HEAP_SIZE`). Channel messages, queued handles, task records and Linux
file tables all live there. Before this change, when an allocation failed,
Rust's allocation-error path panicked the kernel. Any program could get
there:

- **Queues add up.** Each channel queue is bounded (8 messages of up to
  1 KiB each), and each process has 64 handles. But a channel end can
  travel inside another channel's queue, where it no longer counts
  against the sender's handles. One process can park 30 holder channels ×
  16 ends × 8 KiB, about 3.8 MiB. Three such processes are more than the
  heap.
- **Ends could be parked where no close reaches them.** Send end `b` over
  its own peer `a`, then close `a`. Closing `a` frees the queue towards
  `a`, but `b` waits in the queue towards `b`, inside the channel it keeps
  alive. Nothing can ever free it. The same happens when two channels each
  carry the other's end. In a loop, this leaks the heap one channel at a
  time, with only two handles open at any moment.

## The rules

1. **A reserve.** `heap::RESERVE` (1 MiB) is kept for the kernel's own
   needs. These requests from a program fail with `NOMEM` once taking them
   would eat into it:
   - `CHANNEL_CREATE`;
   - `SEND` (counting the message's length);
   - `SEND_HANDLE`;
   - `SPAWN`.

   The kernel's own allocations (starting and ending tasks, file service
   messages, the shell) may still use the reserve. So the program that
   fills the heap gets an error it can handle, and the kernel stays up.
2. **No parking a channel inside itself.** `SEND_HANDLE` refuses:
   - an end sent over its own channel, with `INVAL`;
   - an end whose channel's queues already carry a handle, with `BUSY`.

   A cycle needs some channel that carries a handle to be sent somewhere.
   The last send that would close the loop is always refused, so every
   queued end can still be reached by closing a handle someone holds.

## Why not per-process quotas

The audit item was worded as "a bounded kernel heap per process". A quota
needs every allocation charged to a process, and the charge moved when a
message or handle changes hands. The reserve is simpler, and it protects
the property that matters, which is that the kernel does not panic. It
does not stop one program from crowding others out of the heap until it
exits; the roadmap keeps per-process accounting as a later option if that
is ever seen.

## Checks

`heap-flood` (`cargo xtask verify --case heap-flood`): from the shell, runs
`hare-test` (`user/src/bin/hare-test.rs`), which:

1. sends an end over its own channel, and expects `INVAL`;
2. sends an end whose channel carries a handle, and expects `BUSY`;
3. starts three copies of itself that each park about 3.8 MiB, and expects
   at least one to get `NOMEM`;
4. lets them exit, and expects a channel and a 1 KiB message to work
   again.

The case passes only if `hare-test` exits 0 and the kernel is still running.
