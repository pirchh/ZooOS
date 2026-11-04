# Window server events (2026-11-04)

Status: implemented, the second part of the Bella roadmap's Stage 4 (after
application actions, docs/actions-design.md). Lifting the fixed
five-application arrays is not done.

## 1. Subscribing

- An application sends `events:on` (`Window::subscribe`); from then on the
  window server sends it `event:<what>` for:
  - everything it already records for Bella's screen snapshot
    (docs/context-design.md): opened, closed, minimized, maximized, put
    on the left/right, opened a file in, asked an app to;
  - **focus changes** (`focused Notes`), which go to subscribers only, so
    the snapshot's "recently" list stays as it was.
- **The window server never waits for a subscriber.** A full queue misses
  the event; the misses are counted and logged at 1, 2, 4, 8, ... (the
  roadmap's "drop when a subscriber falls behind").
- A subscription ends with the application.

**Replies are never crowded out.** The first version lost a reply: in the
full suite, `bella-tiling`'s three-column split sent Bella three
`window-done:` replies and several events at once, her 8-message queue
filled ("Bella missed 1 events"), and the reply "Network is on the right."
was dropped (it was a plain non-blocking send). Now every message the
window server owes an app (replies, `context:`, clicks, menu choices)
goes through that app's held queue (`post`, the keys' queue of
docs/keys-design.md), delivered in order as it reads; events skip an app
that has anything held. `bella-tiling` passes again.

## 2. Bella

- Bella subscribes when she starts and keeps the last 5 events.
- **"it" from what the user did:** in a window command ("maximize it",
  "close that", "minimize it") that names no app, "it" becomes the app
  of the newest event that names one (not Bella herself): the user opened
  Notes from Files, then said "maximize it".
- Only for window commands: "open it" after a search still means the
  file found (docs/talk-design.md §6), and steps of one request use the
  step before (docs/multi-step-design.md).
- The log says which: `Bella: "it" is Notes (from the desktop's events)`.

## 3. Verified

| Case | Result |
| --- | --- |
| `bella-events` (TCG) | `OK`: Notes opened from Files' Applications with the keyboard; a click on Bella; `maximize it` → `Bella: "it" is Notes (from the desktop's events)`, `placed Notes at 24x38 1872x948` |
| `bella-events` with Bella not subscribing (negative control) | `FAIL`: no `"it" is Notes` line within 120 s |

The first version of the case went back to Bella with Tab, which focused
Files on the way, so "it" was Files: the newest event wins. With voice,
Bella never takes the focus.

## Not done

- Events for windows moved or resized by hand, and for the dock.
- Lifting the fixed application arrays (`PROGRAMS`, `WINDOWS`).
