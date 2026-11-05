# The action registry (2026-11-04)

Status: implemented (version 0.115.0). The owner's direction: Bella must
be data-driven, so a new app feature reaches her without changing her. An
app describes what it can do; Bella's commands, prompt, argument reading,
asking and the window server's checks all come from that description.

## 1. Where the registry lives

**One file per app, installed with it:** `etc/actions/<program>.actions`
in the boot image, from `seed/etc/actions/`, the way a macOS app carries
its metadata.
- The registry can't come from running apps announcing themselves: "add
  milk to my notes" must work while Notes is closed.
- Three programs read the files at start:
  - ElephantHandler builds the model's commands from them;
  - Bella reads them to fill slots, ask and dispatch;
  - PeacockHandler checks every `do:` against them.
- **Adding an action** means a few lines in its app's file plus its handler
  in the app. Bella's code doesn't change.

## 2. The format

Plain text, one entry per line, so a person can read and write it:

    # What Notes can do for Bella (docs/registry-design.md).
    app notes
    action write
      about write text down in Notes
      effect change
      slots text
      verbs write down | write | add | put | jot down | take a note that | ...
      places in my notes | to my notes | on my list | ...
    action change
      about change words in Notes
      effect change
      slots old new
      joins to | with | for | into
      verbs change | replace | swap
      places in my notes

| Key | Meaning |
| --- | --- |
| `app <program>` | the program the actions belong to (the window server's name) |
| `action <name>` | starts an action; its name is also the model's command word unless `word` says otherwise |
| `word <word>` | the command word, when it differs (Network's `fetch` is `browse`) |
| `about <text>` | the prompt's description |
| `effect read \| change \| delete` | `delete`: Bella asks first, always |
| `slots <name>...` | the argument's parts, filled from the request's own words; `address` is a web address, anything else is text |
| `verbs`, `places` | words before and after the text that are not part of it ("take", "off my list") |
| `joins` | the words between two slots ("to", "with") |
| `ask <text>` | the question for a `delete`, with `{slot}` filled in |

Lines are at most 200 bytes and a file at most 8 KiB. A malformed entry is
skipped with a log line naming it, and the rest of the file still loads.
A command word whose first token collides with another command's is
skipped too, because the model chooses commands by their first token.

## 3. How a request flows

1. The model chooses among the built-in system commands (files, windows)
   and every registry action's word, by first-token score.
2. **Slots are filled from the request** (`registry::fill`):
   - courtesy ("hey bella", "please", "can you") and the action's verbs
     are taken off the front, its places off the end;
   - two slots split at the first join word;
   - an `address` slot is the request's web address.
3. Bella dispatches by word:
   - `delete` asks with the entry's `ask` text, answered by key or voice;
   - otherwise it sends `do:<program>:<action>:<slots>` at once, with
     slots joined by the unit separator (0x1F).
4. The window server checks the program, the action and the slot count
   against the registry, opens or raises the app, and passes the action
   on.

## 4. What the files declare today

| App | Action (word) | Effect | Slots | Example |
| --- | --- | --- | --- | --- |
| Notes | `write` | change | text | "add flowers, egg and milk to my notes" |
| Notes | `remove` | **delete** | text | "delete flowers, egg and milk from my notes"; "take milk off my list" |
| Notes | `change` | change | old, new | "change egg to bread in my notes"; "replace milk with oat milk" |
| Network | `fetch` (`browse`) | read | address | "go to example.com" |

**What `remove` and `change` do** (`zoo_infer::notes`, applied by Notes):
- **`remove`:** a line with the same words is removed. Otherwise an item
  of a list line is taken off it: "flowers, egg and milk" minus "milk" is
  "flowers and egg". A line with no items left goes.
- **`change`:** changes whole words only ("egg" is not changed inside
  "eggplant").
- **Several matching lines are asked about** (since 0.118.0,
  docs/targets-design.md); before that, the newest match won.
- A line's closing mark is kept: "Shopping: flour and eggs.".

**Asking before a delete:**
- Bella asks with the entry's `ask` text, for example `Remove "milk" from
  your notes?`.
- The answer can be y or n, or spoken. Her question sends `await` to
  BatHandler, which passes on the next utterance within 10 s without "hey
  Bella" (`heard "Yes," as her answer`). "yes", "yeah", "sure", "ok" or
  "do it" is yes; "no", "nope", "cancel" or "keep" is no.

**Typed commands:** a registered word is a typed command when the word
after it is not a sentence word. "write flowers, egg and milk" and
"change egg to bread" are commands; "write me a poem" is a request.

**The gate, from what the cases heard:** "Hey Bella, remove milk." came
out "Pay billah, remove milk.". A 3–4 letter word that sounds like "hey"
(one letter off, or ending "ay"/"ey") now counts as a greeting when the
next word is close to "Bella". "Pay the bill" and "Say hello" do not
count.

## 5. Verified

| Check | Result |
| --- | --- |
| `cargo test -p zoo-infer registry`, `notes` | the shipped files load; every phrasing the hand-written code handled fills the same slots from the files; invocations checked (slot count, unknown actions); bad entries skipped with a reason; newest match, closing marks |
| `bella-notes` (typed, no model) | `OK`: write, change ("egg" to "bread"), remove asks and y takes "milk" off "flowers, bread and milk", n keeps |
| `bella-notes` with only the `remove` entry deleted from `notes.actions` (negative control) | `FAIL`: no question; removing is gone with the data, nothing else changed |
| `voice-confirm` (new): "Hey Bella, remove milk." then "Yes." through the microphone | `OK`: heard "Pay billah, remove milk."; asked; heard "Yes," as her answer; milk taken off the shopping line |
| `bella-actions`, `net-bella-browse` | `OK` through the registry |
| `assist-check` (Qwen3-1.7B, 4 bits) with 5 new requests (the owner's) | 41 of 43 right; all 5 new right, "delete … from my notes" → `remove` (margin 20.04); 1 of 8 no-command requests acted on ("What time is it?" → `close Network`, margin 0.67) |

The prompt lists the registry in file order (`network`, then `notes`).
With Notes first, nothing outside the scope acted, but "take milk off my
list" fell to `none` (margin 0.46): each order has one near-tie. A choice
that close is what "ask when unsure" is for.

## 6. Not done yet (the next steps)

- **Targets:** apps publish what they hold (Notes' titled notes), and a
  `note` slot is matched against it, with "did you mean this note or that
  one?". The format is ready for slot kinds.
- **Pattern learning:** a confirmed request is remembered as a template
  ("add {text} to my notes" → `notes write`), and asking when unsure.
- The built-in system commands (files, windows) still live in code. They
  are the system's own, served by the broker and the window server.

## Requests in the registry's words (0.119.0)

A request that starts with one of an action's verbs and ends with one of
its places of two words or more ("add milk to my notes") runs that
action without the model: `Registry::phrased`, docs/regression-design.md.
