# Bella remembers what she is told

Status: implemented 2026-11-03 (version 0.87.0). The owner: Bella should
"learn with the user", and keep what she learns "into a smart vector store
or something". This is the first step: facts the user tells her, kept on
disk and found again by the words they share with a question. A vector
store comes later (§5).

## 1. What the user can say

| Said to Bella | What happens |
| --- | --- |
| "remember that my dentist appointment is on Friday" (also "remember X", "note that X", "don't forget X") | The fact is kept: "I'll remember that." |
| "what do you remember?" / "what do you know about me?" | The facts are listed |
| "when is my dentist appointment?" | The facts sharing at least two words with the question go into her reply in words |
| Anything answered in words | Facts sharing at least one word go with it |
| "forget my dentist appointment" | "Forget what you told me about …?" `y` forgets; `n` keeps |

**The rules** are in `zoo_abi::bella` (`fact_to_remember`,
`fact_to_forget`, `asks_what_remembered`), with host tests. They come
after typed commands and before small talk and the model.

## 2. Where facts live

- **The file:** `.bella-facts` in home, one fact per line, at most 8 KiB.
  The oldest facts go first, and a repeated fact replaces itself.
- **Who touches it:** OtterHandler, the broker, through four new
  requests: `fact-add`, `fact-find`, `fact-list` and `fact-forget`. The
  model service still holds no file handle.
- **Finding:** `fact-find <question>` scores each fact by the content
  words it shares with the question. Filler words such as "my" and "the"
  are dropped, as in the A2 memory. It returns the best three as `| <score>
  <fact>`.
- **Forgetting** removes every fact sharing at least half of the words
  named. Bella asks first, under the owner's "confirm deletes" rule.

## 3. How a fact reaches a reply

Bella sends `talk <question>\t<facts>`. `Talk::reply_with` puts `(What I
told you before: …)` before the question in the user turn, so the model
answers with them in view. Bella logs `Bella: from what you told me: …`.

**A question that two or more words tie to a fact** goes straight to a
reply in words, without a command choice: "when is my dentist
appointment?" is not a file search.

## 4. Safety

- **Facts are text the user typed.** They reach only the reply model, whose
  output is shown, never run.
- **The file is in home,** so another program could plant a fact. At worst
  that puts words in Bella's mouth, and it can trigger nothing: facts
  never reach the command choice. The broker's own handle for its memory
  files is roadmap Stage 2 work.

## 5. Not done: the vector store

Facts are found by shared words, not by meaning: "my teeth doctor" does not
find "dentist". A vector store needs a small embedding model, which is
another download for the owner to approve, and engine support for its
architecture. The model's own hidden states make poor embeddings without
training. When it comes, it replaces `fact-find`'s scoring and nothing
else.

## 6. Checks

- **`bella-facts`** (TCG):
  1. two facts remembered and listed;
  2. "when is my dentist appointment" logs the dentist fact only;
  3. a new desktop still lists both;
  4. forget with `n` (nothing forgotten), then with `y` (one forgotten,
     one kept).
- **`bella-facts-answer`** (WHPX): told the dentist is on Friday, asked
  when the dentist is, she says ``Your dentist appointment is on
  Friday.`` (953 ms).
  - **Negative control:** the fact not sent with the question → `FAIL`.
    She said ``I'm Bella, not a dentist. I can't provide dental care.``
