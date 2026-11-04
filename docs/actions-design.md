# Application actions (2026-11-04)

Status: implemented, the first part of the Bella roadmap's Stage 4 (the
action and event interface, docs/bella-roadmap.md). Applications declare
what they can do for another; Bella asks; the window server routes. Events
(subscribers) and lifting the fixed app arrays are not done yet.

## 1. The manifest

`zoo_abi::actions::ACTIONS`, static and shared by every program:

| Program | Action | Argument | Effect | What it does |
| --- | --- | --- | --- | --- |
| `notes` | `write` | text | change | adds a line at the end of the open note and saves it |
| `network` | `fetch` | text | read | fetches a page by name or address (plain HTTP) and shows its text |

Each entry has an `Effect` (read, change, delete). The window server
refuses `Delete` actions until they can ask first; none is declared.
Files declares none: Bella's `open` already shows a folder in Files.

## 2. How an action travels

1. Bella sends `do:<program>:<action>:<argument>` on her window channel
   (`Window::act`).
2. PeacockHandler checks it against the manifest
   (`actions::parse`: the action exists, a text argument is given and at
   most 400 bytes). It opens or raises the app as a dock click does,
   remembers who asked (`App::asked_by`), and sends the app's front window
   `do:<action>:<argument>`. Refused: "Error: no app can do that."
3. The app does it and answers `did:<sentence>` (`Window::did`).
4. PeacockHandler passes the sentence to the asker as
   `window-done:<sentence>`, which Bella shows and logs, as for window
   actions (docs/window-actions-design.md). The answer comes from the app
   that did the work.

## 3. Bella

- **Commands:** `write <text>` and `browse <address>`, typed or chosen.
  Qwen3's prompt gains `write <text>: write text down in Notes` and
  `browse <address>: open a web page in the Network app`; for 0.5B they
  are masked like the window actions.
- **The text and the address come from the request** (as for `split`):
  `note_text` drops the asking around it ("hey bella take a note that
  the dentist is on friday" → "the dentist is on friday"; "write down
  buy milk in my notes" → "buy milk"); `address_in` takes the first word
  that is a host name or address.
- **Not remembered for replay:** a learned `write` would write an old
  request's text for a new one.

The first wording of the `browse` line ("show a web page") tipped "Hide
the Network window" from `minimize` to `maximize` (margin 1.6);
"open a web page" put it back (4.6).

## 4. Verified

| Check | Result |
| --- | --- |
| `bella-actions` (TCG, typed) | `OK`: `write down buy milk` → `PeacockHandler: Bella asks Notes: do:write:buy milk` → `Notes: wrote a line in Documents/notes.txt` → Bella shows `Wrote "buy milk" in Documents/notes.txt.` |
| `bella-actions` with the answer not passed back (negative control) | `FAIL`: no `Bella: the window server says: Wrote "buy milk" ...` within 120 s |
| `net-bella-browse` (TCG, isolated network) | `OK`: `browse 10.0.2.100` opens Network, which connects and fetches the harness's page; Bella shows "Fetched 10.0.2.100: 10.0.2.100  HTTP/1.0 200 OK  92 bytes" |
| `bella-actions-model` (`--accel whpx --cpus 8`) | `OK` 3 of 3: ``chose write `the dentist is on friday` `` in 519–530 ms; the screenshot shows the line in Notes |
| `assist-check` (Qwen3) | `PASS`: 36 of 38 (all 4 new), 0 of 8 |
| `assist-check --bella 0.5b` | `PASS`: 18 of 38, 1 of 8 |

## Not done

- Events: subscribers for focus, open, close and minimise.
- More actions (Notes: new note; Files: select; Network: links); actions
  that need confirmation.
- `browse` reaches only plain-HTTP sites: TLS needs the owner's approval
  to add crypto crates.
