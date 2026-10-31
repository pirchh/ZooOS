# Linux step 7 (L7): Unix sockets, descriptor passing and event waits

Status: implemented 2026-10-31 (version 0.47.0). Follows L6
(docs/linux6-design.md). Wine's server talks to every Windows program over
Unix stream sockets, hands them descriptors with `SCM_RIGHTS`, and waits
with `epoll`/`poll`. Event loops in general use these and `eventfd`.

## 1. Goal

`c/linux/ermine-test.c` (the ermine is an Arctic stoat) is ordinary Linux
C. It checks:
- **Socket pairs:** `socketpair` in both directions, and a pipe passed
  across it with `SCM_RIGHTS`.
- **Named sockets:**
  - A path name is bound, and binding it twice fails with `EADDRINUSE`.
  - `listen`; a forked client connects and sends a message with a pipe
    attached; `poll` sees the pending connection; `accept4`.
  - `SO_PEERCRED` gives the client's pid; the reply reaches the client;
    end of data once it exits; `ECONNREFUSED` after the listener closes.
- **Waits:**
  - `poll`: a 60 ms timeout, then `POLLIN`, then `POLLHUP`.
  - `select`: readable and writable sets, with a zero timeout.
  - `epoll`: an eventfd and a pipe each report their user data;
    `EEXIST`, `EPOLLONESHOT`, re-arming with `EPOLL_CTL_MOD`, and a
    thread waking another thread's `epoll_wait` through an eventfd.
- **Non-blocking and eventfd:** `pipe2(O_NONBLOCK)` giving `EAGAIN`, and
  `F_GETFL`/`F_SETFL`; eventfd counting and semaphore mode.
- **Timing:** a socket ping-pong between two processes.

## 2. Design

- **A connected Unix stream socket is two pipes** from L5, one each way:
  64 KiB rings of physical pages. Reads, writes, end-of-data, `SIGPIPE`
  and wake-ups therefore behave as for pipes. `shutdown` for writing
  closes the outgoing pipe's writer.
- **Names.** A bound name is `@…` (abstract, kept as raw bytes) or `/…`
  (a path, resolved against the working directory). Names live in one
  kernel-wide table and are free again once the socket is closed. Paths
  do not appear in directory listings, and need no unlink.
- **Connecting** to a listening socket creates the two pipes and puts the
  server's end in the listener's backlog. With a full backlog the
  connect waits. Each end records the other's pid (`SO_PEERCRED`).
- **`SCM_RIGHTS`:**
  - `sendmsg` copies the named descriptors and attaches them to the
    first byte it sends: the pipe records each byte count that carries
    descriptors.
  - `recvmsg` stops before the next byte that carries descriptors, so
    each call gets exactly the descriptors sent with its first byte.
    They go into the lowest free slots (`MSG_CMSG_CLOEXEC` honoured);
    those that do not fit the control buffer are closed, with
    `MSG_CTRUNC` set.
  - A plain `read` drops attached descriptors, as Linux does.
- **Readiness** for `poll`, `ppoll`, `select`, `pselect6` and
  `epoll_wait` comes from one function:
  - pipes: data or end for reading, room or no reader for writing;
  - sockets: a listener with pending connections; a connected socket by
    its two pipes, plus `POLLRDHUP` when the peer is gone;
  - eventfds: count above 0, or room left;
  - files and the console: always ready.
- **Waiting:** a wait with nothing ready blocks until the system's next
  wake-up (any pipe, socket, eventfd or process change wakes waiting
  calls) or the timeout. Signals interrupt with `EINTR`; waits never
  restart. The signal mask argument of `ppoll`/`pselect6`/`epoll_pwait`
  is not applied.
- **`epoll`** keeps (descriptor, events, data, disabled) entries:
  - Edge-triggered entries are treated as level-triggered.
  - `EPOLLONESHOT` disables an entry after it reports, until
    `EPOLL_CTL_MOD`.
  - Closed descriptors are dropped from the set when the next wait meets
    them.
- **`eventfd`/`eventfd2`**: an 8-byte counter, with semaphore mode and
  `EFD_NONBLOCK`.
- **Non-blocking descriptors:** each process keeps a non-blocking bit per
  descriptor. It is set by `O_NONBLOCK` (`open`, `pipe2`, `socket`,
  `accept4`, `eventfd2`), `fcntl(F_SETFL)` and `ioctl(FIONBIO)`, and is
  copied by `dup` and `fork`. A pipe, socket or eventfd call that would
  block returns `EAGAIN` instead, as does `MSG_DONTWAIT`. Waits for
  BadgerHandler are never turned into `EAGAIN`.
- Also: `ioctl(FIONREAD)`, `getsockname`/`getpeername`, `getsockopt`
  (`SO_TYPE`, `SO_ERROR`, buffer sizes, `SO_ACCEPTCONN`, `SO_PEERCRED`),
  `setsockopt` (accepted), `sendto`/`recvfrom` on connected sockets, and
  `MSG_NOSIGNAL`.

## 3. Verification and measurements

`linux-ermine` requires the program's lines and its two children (the
socket client, the echo child), each exiting 0, then `ErmineTest: all
checks passed`.

**Measured** (WHPX, 1000 one-byte round trips between two processes on a
socket pair, three runs): 8933 / 8956 / 9037 cycles per round trip. Each
round trip is two writes, two reads and two task switches.

Negative controls (each restored after):

| Change | Result |
| --- | --- |
| Descriptors sent with `SCM_RIGHTS` are dropped | `FAIL`: exit 15 (`recvmsg SCM_RIGHTS`) |
| Poll-style timeouts end at once | `FAIL`: exit 23 (`poll on listener`) |
| `EPOLLONESHOT` not honoured | `FAIL`: exit 87 |
| Non-blocking descriptors block anyway | `FAIL` (timeout) |

## 4. Not in step 7

- Datagram and seqpacket sockets; `SCM_CREDENTIALS`; `MSG_PEEK`,
  `MSG_WAITALL`.
- Internet sockets and networking (needs a network driver).
- `timerfd`, `signalfd`, `inotify`.
- True edge-triggered `epoll`; nested epoll sets; applying the signal
  mask argument of `ppoll`/`pselect6`/`epoll_pwait`.
- Socket files in the file system.
