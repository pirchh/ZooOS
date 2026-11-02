# Network step 2 (N2): TCP, and a first HTTP request

Status: implemented 2026-11-02 (version 0.67.0, in the same PR as N1).
It adds a TCP client to the stack, so the guest can hold a conversation
with another machine. The check is an HTTP request to a server on the
host.

## 1. TCP client (`zoo_rt::net`)

- **Connect:**
  - a three-way handshake: SYN, SYN-ACK, ACK;
  - the SYN is resent up to 4 times;
  - a reset answers "connection refused".
- **`write`:** segments of at most 1460 bytes, one in flight. Each is
  resent until its acknowledgement arrives, up to 4 times.
- **Receiving:**
  - data is taken in order;
  - anything out of order is dropped and acknowledged with where we are,
    so the sender resends it;
  - a FIN marks the other side finished;
  - `read_to_end` collects until then, or until nothing arrives for a
    while.
- **`close`:** sends our FIN and waits briefly for its acknowledgement.
- **Checksums** include the IPv4 pseudo-header.
- **Not implemented:**
  - windows beyond one segment in flight;
  - congestion control;
  - listening for incoming connections;
  - timestamps, window scaling and SACK.

  These are the next things a network service needs.

## 2. The check: an HTTP request without the internet

- **The server:** for the `net-http` case the harness starts a one-thread
  HTTP server on `127.0.0.1` at a free port. It answers every `GET` with
  `Hello from the host, ZooOS!` and stops after the case. Its waits are
  bounded: accept polls every 20 ms, and reads time out after 5 s.
- **The route:** QEMU's isolated user network gets
  `guestfwd=tcp:10.0.2.100:80-tcp:127.0.0.1:<port>`, so the guest reaches
  that server at 10.0.2.100:80 and nothing else.
- **GyrfalconTest** (`user/src/bin/gyrfalcon-test.rs`):
  - runs DHCP and resolves the gateway;
  - connects;
  - sends `GET /zoo HTTP/1.0`;
  - reads the whole answer and closes.

  It exits 0 only for `200 OK` with exactly that body. The harness prints
  how many requests the server answered.
