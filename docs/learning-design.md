# Bella learns from requests

Status: **work in progress, not merged** (branch `bella-learn`). The first
version below was measured and found wanting (see "Measured, and what
changes"); it waits for multi-core. The owner asked for Bella to "train
with the user's requests" (2026-11-03), with answers as fast and accurate
as possible. Their standing preference (2026-11-02) is a frozen base model
with an example memory. This is that: Bella learns by remembering, not by
changing the model's weights.

## Why not change the weights

Training a model on the device needs a training engine: gradients,
optimiser state, several times the memory of inference. ZooOS does not
have one. Training also risks making the model worse in ways no check
here would catch. An example memory changes nothing in the model, can be
read and edited (`.bella-memory` is a text file), and takes effect at
once.

## What Bella already kept

When the model's command answers a request, the broker keeps
`request<TAB>command` in `.bella-memory` (docs/a2-design.md). A command
the user types after a wrong choice answers the earlier request, and is
kept the same way: that is how a correction is learned. Until now these
were found again only by shared words (`recall`, 75 % alike).

## By meaning

MagpieHandler (docs/vectors-design.md) embeds each request in
`.bella-memory`, as a query. It refreshes them before each use; requests
already embedded keep their vectors.

**`example <request>`** answers with up to 3 past requests at least 0.6
alike, best first: `| <score in thousandths> <request><TAB><command>`.

Bella asks it after the word recall finds nothing:

1. **Reuse,** without the model, when all three hold:
   - the best is at least **0.870** alike (`REUSE_AT`);
   - its command **changes no file** (`search`, `list`, `inspect`, `read`,
     `open`, `launch`, `info`);
   - every word of the command's argument (3 letters or more) is **in the
     new request**.

   It logs `Bella: reused by meaning `<command>` from `<request>``.
2. **Otherwise,** the past requests go to the model's command choice as
   examples: `Earlier: "<request>" was done with `<command>`; …` (at most
   300 bytes). The model decides, helped by what worked before.

## The threshold, measured

`cargo xtask embed-check` prints these pairs (host, Qwen3-Embedding-0.6B,
queries, 256 dimensions):

| Pair | Alike | Same request? |
| --- | --- | --- |
| "Find my pancake recipe" / "where is the pancake recipe" | 0.9515 | yes |
| "Find my pancake recipe" / "I need the pancake recipe again" | 0.9457 | yes |
| "open my notes" / "open the notes file" | 0.9284 | yes |
| "list my music" / "what music do I have" | 0.8782 | yes |
| "Find my pancake recipe" / "look up my recipe for pancakes" | 0.8782 | yes |
| "Find my pancake recipe" / "where did I put the recipe for pancakes" | 0.8655 | yes |
| "trash old.txt" / "trash new.txt" | **0.8408** | **no** |
| "move notes.txt to Documents" / "move notes.txt to Music" | **0.8302** | **no** |
| "Find my pancake recipe" / "search for pancakes" | 0.8010 | yes |
| "open my notes" / "trash my notes" | 0.7843 | no |
| "how much space am I using" / "how full is my disk" | 0.7835 | yes |
| "show me my photos" / "list the Photos folder" | 0.7726 | yes |
| "list my music" / "list my documents" | 0.6487 | no |
| "open my notes" / "open my photos" | 0.6294 | no |
| "Find my pancake recipe" / "find my tax forms" | 0.2762 | no |

**No threshold separates them alone.** Different requests reach 0.84 when
only a file name changes, and the same request in other words can be as
low as 0.77. So:
- **0.870** is above every "no" pair measured;
- the argument check stops a file name from standing in for another;
- destructive commands are never reused;
- below the threshold the model decides, with the examples.

## Measured, and what changes

`bella-learn` (WHPX, one core) passed, but showed two problems:

- **Reuse was slower than the model.** "look up my recipe for pancakes"
  was reused correctly (878 of 1000 alike), but answered in 1,565 ms,
  against 1,191 ms for the model. Finding past requests embeds the new one
  first, about 0.8 s on one core, and on one core that time adds to every
  request.
- **Examples misled the model.** For "look up my recipe for waffles" (605
  alike), the model was given the pancake example and chose `read pancake
  recipe`.

**Next version:**
- **Race instead of asking first.** Bella asks the model and the example
  memory at the same time, and takes the memory's answer only if it is
  reusable and comes first; the model's request is then cancelled. This
  adds no time, but needs a second core for the memory to win: it comes
  after multi-core.
- **No examples to the model:** measured, they made choices worse.

## Checks

`bella-learn` (WHPX, with the embedding model):

1. "Find my pancake recipe" is answered by the model and learned.
2. "look up my recipe for pancakes" (0.878 alike) logs `reused by meaning
   `search pancake recipe``, then the time it took.
3. "look up my recipe for waffles" is not reused: the model chooses, with
   the example.

## Not done

- **No forgetting by meaning.** A wrong memory is fixed by typing the
  right command, which replaces it, or by editing `.bella-memory`.
- **Talk is not learned:** only command choices are.
