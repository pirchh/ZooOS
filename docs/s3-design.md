# Services step 3 (S3): handles in messages, services found by name

Status: implemented and verified 2026-10-30 (results in docs/verification.md).
Builds on S1 and S2. Passing handles over channels is also the IPC feature Wine's
`wineserver` needs (docs/feasibility/wine-hosting.md, "descriptor passing").

## 1. Handles in messages

- A channel message is now either bytes (as before) or **one handle plus
  a 64-bit tag**.
- `send_handle(chan, handle, tag)` (call 49) **moves** the handle: it
  leaves the sender's table and travels in the queue.
- `recv_handle(chan, out)` (call 50) takes the next message if it is a
  handle message, inserts the handle into the receiver's table and writes
  `{handle, tag}` to `out`. Blocking, like `recv`; with flag
  `RECV_NONBLOCK` (third argument) it returns `AGAIN` instead
  (`zoo_rt::try_recv_handle`). ABI version 11.
- `recv`/`try_recv` on a handle message, and `recv_handle` on a bytes
  message, return the new error 19 `WRONGKIND` and leave the message
  queued, so a protocol mistake cannot lose a handle.
- When a channel end closes, handles still queued towards it are released
  (a channel end inside a closed queue closes too, so peers see
  `PEERCLOSED`).
- Rights are unchanged by the move; a process can only send handles it
  holds.

## 2. Finding services by name

- Each service already has a channel to Zookeeper. It asks
  `connect <name>`; Zookeeper creates a fresh channel, sends one end to the
  named running service (tag 1 = "new client") and the other end back to
  the requester (tag 2 = "your connection"), or replies `err <reason>`.
- Services accept clients by taking handle messages on their supervisor
  channel and then serving each client channel.
- Only processes Zookeeper supervises can ask in S3; apps get the same
  through the desktop session later.

## 3. Verification

- `poke seal-test connect`: seal-test asks for `beaver-handler`, receives
  the connection, sends `dump seal` directly to BeaverHandler, and logs how
  many lines came back. The test requires that log line (through
  BeaverHandler itself) and that BeaverHandler still answers Zookeeper's
  pings.
- A boot test (Arctic name: `ptarmigan-test`) checks the mechanics:
  moving a channel end through a channel and using it, the moved handle
  being gone from the sender, `WRONGKIND` in both directions with the
  message preserved, and handles in a closed queue being released (the
  peer sees `PEERCLOSED`).
- Negative controls: a kernel that copies instead of moving the handle,
  and one that drops queued handles without releasing them.
