# Layouts with try-and-confirm (2026-11-04)

Status: implemented (version 0.117.0). The owner asked: "say i say hey
split these in rows, shouldn't it stack them then say is that what you
meant and then i confirm and it commits that to memory?" Yes, but
learning can only map words onto abilities that exist, so the window
server first needed one general way to arrange windows. Then every
arrangement is a description, and descriptions can be learned.

## 1. The layout tree (`zoo_infer::layout`)

A small split tree, also suggested by an outside design review:

| Text | Meaning |
| --- | --- |
| `h(a,b,...)` | side by side |
| `v(a,b,...)` | stacked |
| `name:2` | a weight (1–4): `h(notes:2,files)` gives Notes two thirds |
| an app's name | that app's window (opened if need be; the second `files` is its second window; Bella stays one) |
| `*` | the next window in front |
| `...` | every remaining window in front (`v(...)` stacks them all) |

Examples:
- rows: `v(...)`
- quarters: `v(h(*,*),h(*,*))`
- two on top and one below: `v(h(*,*),*)`
- Notes big on the left, the rest stacked: `h(notes:2,v(*,*))`

**Limits:** at most 6 windows and 3 levels of nesting; a split has two
parts or more, or `...` alone. Everything else is refused.

`rects` gives each window its rectangle in the space between the menu bar
and the dock, 12 px apart (the split gap). The last part of a split takes
what rounding leaves, so a layout always fills the space exactly.

## 2. Words to a layout (`layout::from_words`)

| Words | Tree |
| --- | --- |
| "in rows", "stack", "stacked", "on top of each other", "vertically" | `v(...)`, or `v(a,b)` with the apps named |
| "quarters", "quadrants", "corners", "grid", "two by two" | `v(h(*,*),h(*,*))` |
| "N on top and M on the bottom / below" | `v(h(N),h(M))` |
| "big on the left", "one on the left and two stacked on the right" | `h(a:2,v(*,*))`, `h(a,v(*,*))` |
| anything else | `None`: plain columns stay the split's |

Named apps fill the first leaves, `*` the rest.

**Bella reads layouts herself, before the model,** when a request asks for
an arrangement ("split", "stack", "arrange", "tile", "lay out", "put"...)
or answers her "How should they go?". This is quicker, works without the
model, and is exact. The model service does the same when the model
chose a split. In `assist-check`, "put these windows in rows" was a model
near-tie (margin 0.55) and still became `layout v(...)` from the words.

## 3. Try, then confirm, then learn

1. Bella sends `layout:<tree>`. The window server (`arrange`) fills the
   leaves:
   - named apps first, through `nth_window_of`, so a second `files` is a
     second window and Bella is never doubled;
   - then the windows in front, the asker last.

   It places them on the first window's monitor and gives the keyboard
   back to the asker, who is about to ask. A window named twice, or too
   few windows, is said, not guessed.
2. If Bella read the layout from words, she asks **"Is this what you
   meant?"**. The answer can be a key or a spoken yes or no
   (docs/registry-design.md §4).
3. **Yes:** the request is learned as `layout <tree>`, in the general form
   with `*` and `...`. So "put these in rows" works with whatever windows
   are open, not the ones from the first time.
4. **No:** "How should they go? Say rows, columns, quarters, or two on top
   and one below." Her next words are read as a layout, and that
   request's yes **teaches both** phrasings.
5. **A learned phrasing** is recalled and arranged with no question.

This is the one kind of learning that happens without a typed correction:
the user's yes is the confirmation (docs/one-bella-design.md).

## 4. Verified

| Check | Result |
| --- | --- |
| `cargo test -p zoo-infer layout` | parsing and printing; refusals (one part, unclosed, weight 9, seven windows, four levels); quarters and weights tile 24x38 1872x948 exactly; `...` expands; words to trees for every row above |
| `bella-layout` (new, typed, no model) | `OK`: "put these in rows" → `v(...)` placed at 24x38/24x358/24x678, 1872x308 each → asked → y → learned. "split it three ways, two on top and one on the bottom" → `v(h(files,*),*)` ("it" is Files, from the desktop's events) → asked → n → "stack them" → asked → y → both learned. "put these in rows please" → `recalled layout v(...) (100% alike)`, no question. The rejected `v(h(files,...))` was never learned |
| `bella-layout` with the window server refusing layouts (negative control) | `FAIL`: nothing placed |
| `assist-check` | 43 of 45; "put these windows in rows" → `layout v(...)`, "split the screen into quarters" → `layout v(h(*,*),h(*,*))` |

Also fixed here: the shared column geometry and the window server's
`colKofN` stopped at three columns, so a four-app split shared the third.
They now allow four.

## Not done

- Layouts across monitors (one tree per monitor); resizing a layout by
  dragging its dividers.
- Routines: a confirmed sequence of actions saved under a name ("set up
  my workspace"), the next learning step.
