# Services step 4a (S4a): waiting on many channels, faster hand-offs

Status: implemented and verified 2026-10-30 (results in docs/verification.md).
A prerequisite for S4 (the filesystem as a service, docs/s4-design.md):
every file operation will be a request to a service, so service round
trips must be fast. Measured before this step: 5 ticks (50 ms) per request
to BeaverHandler.

## 1. Why requests were slow

1. **Polling.** Services checked their channels, then slept a fixed time
   (Zookeeper 2 ticks, BeaverHandler 5). A request arriving just after the
   check waited for the sleep to end.
2. **Idling at the wrong moment.** When a program blocked or yielded, the
   round-robin scheduler often picked the kernel task next. The kernel task
   only idles (`hlt` until the next interrupt), so the program that had
   just been woken waited for the next timer tick (10 ms).

## 2. Changes

- **`wait_any(handles, count, timeout)`** (call 51, ABI version 12):
  blocks until one of up to 16 handles is ready and returns its index. Ready
  means a channel end with a message waiting or a closed peer, or an ended
  process or thread. `TIMEDOUT` after `timeout` ticks; `WAIT_FOREVER` for
  none; 0 polls once. It uses the same restart-on-wake mechanism as other
  blocking calls (every send, close and exit wakes waiters, which re-check).
- **Zookeeper** sleeps in `wait_any` on its control channel and every
  running service, with a timeout set to the next ping, ping deadline or
  restart. **BeaverHandler** waits on its supervisor channel and clients,
  with a 10-tick timeout to collect new log lines (the log buffer sends no
  notification).
- **Scheduler:** on a voluntary switch (a task blocked, yielded, ended or
  crashed) a ready user task is preferred over the idle kernel task. Timer
  ticks keep plain round-robin, so the kernel shell still gets its turn
  while programs are busy (the keyboard and preemption checks still pass).

## 3. Measured

seal-test times 50 ping round trips to BeaverHandler over a channel it got
by name (`services` case, TCG):

| Version | 50 round trips |
| --- | --- |
| before (sleep polling) | 250 ticks |
| `wait_any` only | 50 ticks |
| `wait_any` + scheduler preference | 0 ticks (all within one tick) |

These are observations under emulation on one host, not a benchmark.

## 4. Side effect found

With faster hand-offs, a kernel line can be printed while the shell prompt
(`zoo> `, no newline) is showing, so they share a line. The verifier's
marker checker already looked for markers anywhere in a line; the
harness's `WaitFor` step required whole-line equality and stalled. It now
accepts the wanted line after one or more prompts (unit-tested).

A second instance of the same effect: after leaving the desktop, the shell
printed `Desktop session ended.` and its prompt as soon as the session's
Zookeeper ended, while the desktop's apps were still exiting, so an exit
line landed after the prompt (the `desktop` case's last screenshot check
failed). The shell now also waits (bounded, 2 s) until only the processes
from before the session remain.
