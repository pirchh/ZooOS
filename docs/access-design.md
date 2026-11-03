# Broad access, confirm deletes

Status: implemented 2026-11-03 (version 0.86.0). The owner's decision,
2026-11-02: Bella "needs to be able to freely have access to everything on
the computer, thats kinda the point". Asked how much freedom, the owner
chose **"broad, confirm deletes"** over "fully free".

## 1. What Bella can do now

| Command | What it does | Asks first |
| --- | --- | --- |
| `search`, `list`, `inspect`, `create` | As before | `create` |
| `read <file>` | Shows a text file's first 12 lines (of its first 4 KiB), and how much more there is | No |
| `open <file or folder>` | A folder in Files, a file in Notes (`open:<program>:<path>` to the window server) | No |
| `move <path> to <path>` | Renames, or moves into a folder; `undo` moves it back | No |
| `trash <file or folder>` | Moves it into the Trash (`.Trash`, as Files does), where Files' Put Back restores it | **Yes**: "Move X to the Trash?" `y`/`n` |
| `launch <app>` | Opens Files, Notes, Network or Crash Test by name | No |
| `info` | The ZooOS version, the date and time, the uptime, and how many files and folders home holds | No |
| `undo` | Undoes the last `create` (empty folders only) or the last `move` | No |

**Nothing is deleted for good.** "Delete" means the Trash, which Files can
put back. Emptying the Trash is still not built.

**Files learns `Event::Open`:** a folder path is shown; a file path is
selected in its folder.

## 2. Names resolved against what exists

The model often writes a loose name: `open notes` for
Documents/notes.txt, or `trash my playlist`. OtterHandler resolves a path
in two steps:
1. **Letter case,** as before (A2b).
2. **For commands about something that already exists,** only when the
   path names nothing: the one file or folder anywhere in home whose name,
   or name without its extension, equals the last part with filler words
   dropped. More than one match counts as none.
   - This applies to `list`, `inspect`, `read`, `open`, `trash` and the
     source of `move`.
   - The log says `` `notes` is `Documents/notes.txt` (found by name) ``.

**`create` and the target of `move` never search,** so a new name is
never turned into an existing thing.

## 3. The model's choices

**The command prompt lists all ten commands plus `none`.** Measured on the
host with 31 requests: the 22 before, 8 for the new commands, and "where is
my music". "Delete all my files" is now in scope, so it was replaced by
"Order me a pizza".

| Prompt | 0.5B: right with a command / acted on of 8 | 1.5B |
| --- | --- | --- |
| The command words only | 15 of 22 / 3, and "Find my pancake recipe" became `inspect` | 16 of 22 / 2 |
| Each command with a few words on what it is for | 13 of 22 / 1, with `list` and `inspect` confused | 17 of 22 / 5 |
| Clearer words for `list`, `inspect` and `launch` | 16 of 22 / 2, but "where is my music" became `move /music to Music` (in `bella-convo`) | 17 of 22 / 5 |
| **`search`: "find where files are"** (chosen; "where is my music" added to the set) | **17 of 23 / 2** | 19 of 23 / 5 |

- **0.5B's misses** are mostly arguments the broker resolves anyway: `open
  my notes` → Documents/notes.txt, `trash my playlist` →
  Music/playlist.txt, `launch Notes app` → Notes.
- **1.5B's extra "acted on anyway"** are all `launch` of apps that do not
  exist (Mail, VLC, Pizza Hut). Bella answers "no app called X"; nothing
  runs.
- **The new bar:** at least 17 of 23 right, at most 2 of 8 acted on.

## 4. Safety

- **Destructive commands ask.** `trash` asks before it moves anything, and
  `create` shows its plan first, as before.
- **Reading files now shows their contents.** The lines go into the
  conversation that Bella's replies continue (docs/talk-design.md §6), so
  text in a file can reach the model. It still cannot act without asking
  where it matters:
  - deletes ask;
  - moves can be undone;
  - replies are only shown.
- **The memory's replay filter** covers the new commands: a planted line
  replaying `trash` still asks.
- **Every move and trash is logged** (`OtterHandler: moved X to Y`, `…
  to the Trash as …`), a first step towards the roadmap's audit lines.

## 5. Checks

- **New case `bella-access`** (TCG, typed commands):
  1. `info`;
  2. `read notes` (found by name);
  3. `open Music` → Files shows Music;
  4. `move` and `undo`;
  5. `trash recipes` with `n` (nothing moves), then with `y` (into the
     Trash);
  6. `launch Notes`.
- **`zoo_abi::bella` tests:** the new commands are typed commands.
- **`assist-check`** on the 30 requests, for both models (§3).

## 6. Not done

- **Emptying the Trash**, and deleting for good.
- **Reading non-text files.**
- **Opening a file in an app other than Notes.**
- **System settings,** which ZooOS has few of.
