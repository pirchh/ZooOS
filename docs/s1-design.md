# Services step 1 (S1): a resident supervisor

Status: implemented and verified 2026-10-30 (results in docs/verification.md).
First step of
the services milestone (MILESTONES.md): ZooOS gains long-running system
services under a supervisor, as ARCHITECTURE.md intended ("first user
process and service supervisor").

## 1. Boot sequence

1. Kernel self-tests, then Zookeeper in **check mode** (unchanged: runs the
   Arctic boot tests, exits).
2. The kernel's leak check (unchanged: every boot-test process has exited
   and no frames leaked).
3. The kernel starts Zookeeper in **supervise mode** with a control
   channel (first message `supervise`). It stays running.
4. `READY`, then the shell.

Keeping the check run and the resident supervisor as two instances keeps
the leak check exact: resident services hold memory on purpose.

## 2. Service list

`etc/services` in the initrd (from `seed/etc/services`): one service per
line, `name max_restarts`. S1 ships:

    beaver-handler 3
    seal-test 3

## 3. Supervision

- Each service is started with a channel to Zookeeper as its start-up
  handle. Services answer `ping` with `pong`. A new system call,
  `process_id(handle)` (46, ABI version 9), lets Zookeeper report pids.
- The kernel waits (bounded, 5 s) for Zookeeper's `ready` message, sent
  once its services have started, before printing `READY`, so the start-up
  order is always the same.
- Every 100 ticks (1 s) Zookeeper pings each running service. No `pong`
  within 200 ticks: the service is **unresponsive**, killed and restarted.
- A service whose channel closes has ended; `wait` gives the reason
  (crashed, killed, exit code). It is restarted after a back-off of
  `10 × 2^n` ticks, up to `max_restarts` times; then it is **failed** and
  left stopped.
- Every event (started, crashed, unresponsive, restarted, failed) goes to
  BeaverHandler and to the serial log.

## 4. Services in S1

| Service | Role |
| --- | --- |
| **BeaverHandler** (`beaver-handler`) | Keeps the last 64 log lines it is sent (`log <text>`) with their tick; answers `dump` with them. Zookeeper sends it every supervision event. |
| **seal-test** | Arctic test service: answers `ping`; on `crash` it faults, on `hang` it stops answering. Exists so restarts and health checks can be tested without debug hooks in real services. |

## 5. Shell commands

The kernel shell sends requests on the control channel and prints the
reply lines (bounded wait, 3 s):

| Command | Shows |
| --- | --- |
| `services` | one line per service: `name pid=N state restarts=R last=reason` |
| `logs` | BeaverHandler's recent lines |
| `poke <service> crash\|hang` | forwards a test request (only `seal-test` accepts) |

## 6. Verification

New `services` case (normal image): the resident Zookeeper and both
services start; `services` lists them running; poking seal-test `crash`
leads to a crash line, a restart and `restarts=1 last=crashed`; `hang`
leads to a kill by Zookeeper, a restart and `restarts=2
last=unresponsive`; two more crashes exhaust the limit and the state
becomes `failed` with no further restart; `logs` shows the events.
Negative controls: a supervisor that never pings (hang not detected), and
one that ignores the restart limit.

## 7. Not in S1

Services requesting other services by name, per-service rights in the
manifest, routing every program's `log` output through BeaverHandler, the
desktop session as a supervised service (it still uses its own Zookeeper
instance), and persistence of logs.
