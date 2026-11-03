# Bella sees the screen

Status: implemented 2026-11-03 (version 0.88.0). The first piece of Stage
4 of docs/bella-roadmap.md, "Bella can watch what happens", built the
simplest way that answers the questions people ask.

## 1. What it answers

"What am I working on?", "what's open?", "which windows are open?", "what
did I just open?" (`zoo_abi::bella::asks_about_activity`, with host tests).

## 2. How: Bella asks, the window server answers

**Pull, not subscribe.** The roadmap proposed a subscriber list in
PeacockHandler, with events sent to Bella as they happen. Bella only
needs the state when such a question is asked, so she asks for it then:
- **Nothing is sent while no one asks.**
- **No queue can fill:** channels hold eight messages.
- **No event can be lost.**

A subscriber list can come when something must react to events as they
happen.

**The exchange:**
1. Bella sends `context?` on her window channel (`Window::context()` in
   `zoo_rt::app`).
2. PeacockHandler answers with one `context:` message (logged as `context
   for Bella`):
   - **the focused window;**
   - **every open window,** marked when minimised. A title is shown only
     where it says more than the app's name (Notes' title is the file it
     has open), to keep the snapshot short.
   - **the last six things that happened:** `opened <app>`, `closed
     <app>`, `opened <path> in <app>`.
3. Bella logs `Bella: the screen: …` and asks the model for a reply in
   words. The snapshot goes as context, prefixed "On my screen:" (in the
   user's voice), with any facts (docs/facts-design.md). The model's
   context label is now `(What you know: …)`, for facts and the screen
   alike.

**Wording mattered, measured with `bella-context-answer`:**

| Snapshot as sent | Bella said |
| --- | --- |
| Bare, with every title repeated | "I am working on "zoo.rs" in Notes." |
| "On the user's screen: …" | Cut off at the 300-byte context limit; she said "You are Bella, the assistant built into ZooOS…" |
| Shorter (titles only where they add something), "User's screen: …" | "You are Bella, … You are focused on … `zoo.rs`." |
| **"On my screen: …"** (chosen) | **"You are working on `zoo.rs`, a file that is currently open in the "Notes" application."** |

**Version 0.92.0:** a window's title is now written "Notes showing zoo.rs"
instead of "Notes (zoo.rs)". With the old form the 1.5B model said "You
are working on the "Notes" file, which is the most recently opened
document" and failed the case. With the new form:

| Model | Bella said | Time |
| --- | --- | --- |
| 0.5B | "You are working on `zoo.rs`, a file that is currently open in the `Notes` application." | 2.2 s |
| 1.5B | "You are working on the file "zoo.rs" in the Documents folder." | 8.9 s |

## 3. Safety

**Window titles and file paths reach the model.** Its reply is only
shown. Window contents never reach it.

## 4. Not done

- **Events as they happen.** Bella does not notice anything unless asked.
- **Apps describing their own state** (Notes: "unsaved changes"; Files: the
  folder shown) beyond their titles.
- **Typed actions for apps:** the rest of Stage 4.

## 5. Checks

- **`bella-context`** (TCG):
  1. `open Documents/zoo.rs` (Notes opens it);
  2. click back to Bella;
  3. "what am I working on?" → `context for Bella`, then `the screen:
     focused: Bella …` naming `opened Documents/zoo.rs in Notes`.
- **`bella-context-answer`** (WHPX): the same, and her reply names `zoo`.
  - **Negative control:** no snapshot sent → `FAIL`; she said "I am
    Bella, the assistant built into ZooOS, an operating system."
