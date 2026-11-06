# Titled notes (2026-11-06)

Status: implemented (version 0.133.0). Until now Notes held one note,
`Documents/notes.txt`, and "add milk to my shopping list" went to it or
to the model. Now a request can name a note by its title, and Notes
keeps each titled note as its own file.

## 1. What a titled note is

- **A file in `Documents/Notes/`**, named after its title:
  `Documents/Notes/Packing.txt`. Files shows them, and they open in
  Notes like any text file. `Documents/notes.txt` stays the untitled
  note, for requests that name none.
- **Made when first written to.** "Add passport to my packing list" on
  a system with no Packing note makes `Documents/Notes/Packing.txt`
  (the title's first letter upper case) and writes the line. A remove or
  a change never makes a note: "there is no note called "holiday"".
- **Named in any case, or by some of its words.** An exact title wins
  ("packing" is Packing). Otherwise a note whose title has all the words
  said ("rome" is "Packing for Rome"). With several such notes, Notes
  changes nothing and says which: `which note, Packing for Rome or
  Packing for Oslo? Say its whole title.`
- A title is letters, digits, spaces and `-`, at most 40 bytes, so it
  can never name a path outside the folder.

## 2. How a request names one

`zoo_infer::notes::titled_place` reads the end of the request:

```
<to|into|on|onto|in|from|off|out of> <my|the> <title, 1-3 words> <note|notes|list>
```

It keeps the title and puts back the place as the registry spells it:

| Said | Title | Routed as |
| --- | --- | --- |
| Add passport to my packing list | packing | Add passport to my list |
| remove socks from the packing note | packing | remove socks from my notes |
| put sunscreen on my summer holiday list | summer holiday | put sunscreen on my list |
| change socks to boots in the packing notes | packing | change socks to boots in my notes |
| take socks out of my packing note | packing | take socks out of my notes |

**Everything else about the request is unchanged:** the steps, the
registry's words (docs/regression-design.md), the model and the
questions all see "add passport to my list". Words that are never a
title ("my", "notes", "app", …) keep the registry's own places as they
were: "to my notes", "in the notes app", "to my notes list". A title of
four words or more is not read, and a place that is not a note's
("my workout playlist") is not touched.

## 3. Carrying it to Notes

Bella keeps the title for the request (`note_in`). When the action it
ends in is one of Notes', the invocation gets one more part after the
slots, `^<title>`, beside the request id `#<n>` and a chosen line
`@<line>` (docs/targets-design.md):

```
do:notes:write:passport␟^packing␟#1
```

`zoo_infer::registry::is_extra` treats it as not a slot, so the window
server's check of the slots is unchanged, and `note_of` reads it. Notes
opens the note (saving what was open first), then does the action in it,
so the window shows the note it changed. Delete questions name the note:
`Remove "passport" from your packing note?`

## 4. Verified

| Check | Result |
| --- | --- |
| `cargo test -p zoo-infer` (`titled_places`, `picks_a_note`, `new_titles`, the registry's extras) | pass |
| `bella-titled` (QEMU, no model) | see docs/verification.md |
| `bella-titled` with Bella not adding `^<title>` (negative control) | see docs/verification.md |
| `bella-regress` | the four new sentences as in §2; the others unchanged |

## 5. Not done

- **Opening a note by its title** ("open my packing list"): Files opens
  it today. Bella's `open` is for apps.
- **Reading a note aloud** and listing the notes.
- **Choosing between notes by voice** when several match: Notes says
  which, and the request is said again with the whole title.
- **The model is not told titles exist.** A request whose place isn't in
  the pattern ("jot down socks, it's for packing") goes to the untitled
  note, as before.
