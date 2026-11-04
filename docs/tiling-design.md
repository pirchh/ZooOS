# Tiling: windows side by side (2026-11-03)

Status: implemented. The owner asked for it with window actions ("Notes
on the left and the assistant on the right"), worded freely: "split these
2 windows", "split the screen with the x, y and z apps", "put these
windows side by side", more than one window of one app.

## 1. What Bella does

| Request (examples) | Command | Result |
| --- | --- | --- |
| "put notes on the left and the assistant on the right" | `split Notes and Bella` | Notes on the left half, Bella on the right |
| "can you split the screen with the files, notes and network apps" | `split Files and Notes and Network` | three columns, left to right |
| "hey bella split these 2 windows", "tile my windows" | `split` | the two front windows other than Bella's (with only one other: it and Bella) |
| "put two finder windows side by side" | `split Files and Files` | two Files windows; the second is opened if there is only one |
| "put Files on the right and Notes on the left" | `split Notes and Files` | the sides said win over the order said |

Each app is opened, shown or raised first, as `maximize` does
(docs/window-actions-design.md). The green light gives a window back its
size from before. Columns are the zoomed space (between the menu bar and
the dock) cut into 1 to 3 parts, `SPLIT_GAP` (12 px) apart
(`zoo_abi::ui::column`): at 1920x1080, halves are 930 px wide, thirds 616.

## 2. The model chooses, the request names

**The model decides that a request is a split; Bella reads the apps from
the request itself** (`assist::split_request`), and the model writes no
argument for `split`. Measured first the other way: with the model
writing the apps, Qwen3 put them in its prompt's order ("I want Notes
and Files next to each other" → `split Files and Notes`; "put notes on
the left and the assistant on the right" → `split Files and Bella`), and
named apps for "split these 2 windows". The model is good at what was
asked, and the words already say which apps.

`split_request`:
- takes app names in the order spoken: Files (or "Finder"), Notes,
  Network, and Bella ("assistant", "yourself", or "Bella" after the first
  two words, so "hey Bella, ..." is not a name; "you" is not, since "can
  you split" is common);
- "two"/"2"/"both"/"three" before a name repeats it: that many windows of
  the app;
- "left", "middle", "right" after a name place it; used only when every
  app has one;
- more than three: no command.

Typed `split ...` goes through `split_apps`, the same names without the
sides.

**The window server** gets `window:col<k>of<n>:<program>[#<m>]` (column
`k` of `n`, the app's `m`th window) or `split-front`, and answers
`window-done:<sentence>` as for the other window actions.

## 3. Measured

`assist-check`, Qwen3-1.7B (4 bits):

| | With a command right | Without one, acted on |
| --- | --- | --- |
| Before (window actions) | 25 of 27 | 0 of 8 |
| Model writes the apps | 26 of 33 | 1 of 8 ("What time is it?" → `info`) |
| **Apps from the request; `info` described as "this computer's storage and memory"** | **32 of 34** (all 9 tiling requests) | **0 of 8** |

0.5B: 18 of 34, 1 of 8, as before (window words masked for it).

`bella-tiling-model` ("put notes on the left and the assistant on the
right", 8 processors, WHPX): 3 of 3 `OK`, answered in 781, 835 and 837
ms.

## Not done

- Moving a window to another monitor (one monitor so far).
- Rows (top and bottom), quarters, or more than three columns.
- "These windows" means the front ones, not ones the user points at.
