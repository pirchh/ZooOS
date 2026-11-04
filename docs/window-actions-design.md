# Window actions (2026-11-03)

Status: implemented for `maximize`, `minimize` and `close`. This is the
first step towards the owner's voice-control goal ("Hey Bella, open Notes
and maximize it on my screen"). The order after it: requests of several
steps, then more than one monitor, then the browser, then voice.

## 1. What Bella can do

| Command | What happens | Bella then says |
| --- | --- | --- |
| `maximize <app>` | opens the app if it is closed, shows it if minimised, brings it to the front, and fills the space between the menu bar and the dock (as the green light does) | "Notes fills the screen." |
| `minimize <app>` | hides its frontmost window in the dock (the yellow light) | "Minimized Notes; it is in the dock." or "Notes has no window showing." |
| `close <app>` | quits it (the red light) | "Closed Notes." or "Notes is not open." |

`<app>` is Files, Notes or Network (`LAUNCHABLE`, the same list as
`launch`), with "the" and "app" dropped. Bella herself and Crash Test
cannot be named.

**"Open Notes and maximize it" is one command** (`maximize Notes`),
because `maximize` opens the app first. Requests that really need two
commands wait for multi-step requests.

## 2. How it travels

1. Bella sends `window:<action>:<program>` to PeacockHandler on her window
   channel (`zoo_user::app::Window::window`), beside `launch:` and
   `open:`.
2. PeacockHandler picks the app's frontmost window (`front_window_of`)
   and does what the light or the dock would do (`window_action`): the
   same functions, so the animations and the dock match.
3. It answers `window-done:<sentence>`, which Bella shows and logs. The
   answer comes from the window server, so Bella never claims something
   it did not do ("Notes is not open").

Any application may send it, as any may send `launch:`; only Bella
does.

## 3. The model's choice

The three words are new tools (`Tool::Maximize`, `Minimize`, `Close`). As
for every tool, the model picks one by comparing the scores of the words'
first tokens, then writes the app name, which must be in `LAUNCHABLE`; a
name that is not is no command. Typed as commands (`maximize notes`), they
skip the model.

`assist-check` gained four requests: "Open notes and maximize it on my
screen", "Make Files fill the whole screen", "Hide the Network window",
"Quit Notes".

| Model | With a command right | Without a command, acted on |
| --- | --- | --- |
| Qwen3-1.7B, before | 20 of 23 | 0 of 8 |
| **Qwen3-1.7B, with window actions** | **25 of 27** (all four new ones) | **0 of 8** |
| 0.5B, window actions in its prompt (not kept) | 19 of 27 | 4 of 8 ("Shut down the computer" → `close Network`) |
| 0.5B, kept | 18 of 27 (the four new ones are not chosen) | 1 of 8 |

**0.5B's prompt does not list them,** and their words are masked out of
its choice (`Bella::windows`). It still runs them typed.

## 4. Speed

"open notes and maximize it on my screen", Qwen3-1.7B (4 bits), 8
processors, WHPX: Bella answered in 553, 565 and 591 ms (3 runs; the
model read 17 tokens in about 510 ms and wrote 1).

## Not done

- No `restore` (un-maximise) or `move to monitor N` yet; one monitor.
- One command per request: "open Notes and Files" does one of them.
- Window actions are not in the action manifest the roadmap describes
  (Stage 4: apps declaring their own actions); they are the window
  server's.
