# Bella answers from your files

Status: implemented in version 0.94.0. The word lookup is the fallback
of the file index by meaning (docs/vectors-design.md). This is the first
piece of the knowledge track (docs/bella-roadmap.md): Bella answers from what the user
has written, not only from the model's own knowledge. The owner's vision
is a "smart vector store". That needs an embedding model, which is a
download for the owner to approve. Until then, matching words stands in
for it.

## The flow

1. **Routing.** `zoo_abi::bella::asks_my_files` (host-tested) needs both
   of these, and the request must not be a typed command:
   - something about the user's files: "my notes", "my files", "my
     documents", "did I write", "I wrote"…;
   - something asking what they say: "say", "about", "mention",
     "anything", "what", "when", "who".

   So "where are my files" still goes to `search`, and "open my notes
   file" stays a command.
2. **Said at once.** One of "Let me check your files...", "Looking
   through your files, one sec..." or "Checking what you wrote..."
   (docs/stream-design.md).
3. **Lookup.** The broker's new `lookup <question>` tool:
   - takes the question's words, without filler ("the", "my") or asking
     words ("what", "say", "notes", "about"…), keeping words of 3 letters
     or more;
   - reads up to 200 of the home folder's text files, 16 KiB each,
     skipping hidden ones, so Bella's own `.bella-*` files are never read;
   - scores each line by how many of the words it contains;
   - sends back the best 3 as `path: line`.

   It logs `OtterHandler: lookup `<words>` found N lines in M files
   read`. It is not a change, so its audit line stays in the log only.
4. **No lines:** Bella says "I couldn't find anything about that in your
   files.", and no model runs.
5. **The answer.** At most 900 bytes of lines go to ElephantHandler as
   `answer <question><TAB><lines>`. `Talk::answer_from` asks for an
   answer "using only these lines", or to say they do not answer it. The
   lines are marked as data, and this is a call of its own: as with
   summaries (docs/summary-design.md), nothing of the files stays in the
   model's conversation. Bella notes the question and answer.
6. **Where it came from.** Under the answer, Bella shows "(from
   Documents/notes.txt)".

## Why words, not vectors, for now

| | Word lookup (now) | Embedding vectors (later) |
| --- | --- | --- |
| Download | none | an embedding model (owner's approval) |
| Finds "vet" in "Call the vet about Zara" | yes | yes |
| Finds "doctor" for "physician" | no | yes |
| Cost per question | reading the files, a few ms | an embedding per line, stored |

The flow does not change when vectors come: only the broker's `lookup`
does.

## Measured

- **`bella-lookup` (TCG):** the broker read 6 files and found the one vet
  line.
- **`bella-lookup-answer` (WHPX, 0.5B):** "The vet is called for Zara the
  zebra." in 2.5 s; its first words came at 2.1 s.

## Checks

- **`bella-lookup`** (TCG):
  - "what do my notes say about the vet?" → `lookup `vet` found 1 lines`,
    then `Bella: answering from Documents/notes.txt`;
  - "did I write anything about unicorns" → `found 0 lines`, then `Bella:
    nothing in the files for that`.
- **`bella-lookup-answer`** (WHPX): the answer names Zara or the zebra.
  The file's lines are not in the log, so those words can only come from
  the file through the answer.

## Not done

- Words must match as written: no stems ("vets" does not find "vet"), no
  synonyms.
- Only the first 16 KiB of a file is read, and only 200 files.
- PDFs and other formats are not read, only UTF-8 text.
