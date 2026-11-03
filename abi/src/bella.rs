//! How Bella routes a line typed into her window (docs/talk-design.md): a
//! typed command, small talk answered in words, or a request for the
//! model. Pure text rules, kept here so the host can test them.

/// The commands the user can type (docs/access-design.md adds `read`,
/// `open`, `move`, `trash`, `launch` and `info`).
pub const COMMANDS: [&str; 11] = [
    "search", "list", "inspect", "create", "undo", "read", "open", "move", "trash", "launch",
    "info",
];

/// Words that make a line a sentence rather than a typed command:
/// "search for my music" is a request in words, "search pancake" a command
/// (docs/talk-design.md).
pub const SENTENCE_WORDS: [&str; 20] = [
    "a", "an", "the", "my", "me", "i", "for", "of", "to", "please", "can", "you", "could", "would",
    "where", "what", "is", "are", "and", "tell",
];

/// Whether `text` is a typed command: a command word, then a path or
/// search words, and none of `SENTENCE_WORDS`.
pub fn typed_command(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if !COMMANDS.contains(&first) {
        return false;
    }
    if first == "undo" || first == "info" {
        return words.next().is_none();
    }
    // `move <path> to <path>`: there, "to" is part of the command.
    words.all(|w| {
        (first == "move" && w.eq_ignore_ascii_case("to"))
            || !SENTENCE_WORDS.iter().any(|s| s.eq_ignore_ascii_case(w))
    })
}

/// Greetings, thanks and questions about Bella herself: answered in words
/// without asking for a command (docs/talk-design.md). The 0.5B model sends
/// most of them to `search`.
pub fn small_talk(text: &str) -> bool {
    let first = text
        .split(|c: char| !c.is_ascii_alphabetic())
        .find(|w| !w.is_empty())
        .unwrap_or("");
    const GREETINGS: [&str; 7] = [
        "hi", "hello", "hey", "thanks", "thank", "morning", "evening",
    ];
    const ABOUT_BELLA: [&str; 9] = [
        "your name",
        "who are you",
        "what are you",
        "what can you do",
        "who made you",
        "who built you",
        "how are you",
        "what do you do",
        "are you there",
    ];
    GREETINGS.iter().any(|g| g.eq_ignore_ascii_case(first))
        || ABOUT_BELLA.iter().any(|q| contains_ignoring_case(text, q))
}

/// Whether `text` refers back to the conversation ("is that it?", "what
/// about Photos?", "open it"): then the command choice also gets the last
/// exchange (docs/talk-design.md §6).
pub fn follow_up(text: &str) -> bool {
    const BACK: [&str; 12] = [
        "it", "that", "this", "those", "these", "them", "there", "else", "more", "again", "also",
        "same",
    ];
    contains_ignoring_case(text, "what about")
        || text
            .split(|c: char| !c.is_ascii_alphabetic())
            .any(|w| BACK.iter().any(|b| b.eq_ignore_ascii_case(w)))
}

/// A yes/no question about the last answer ("is that it?", "are there
/// more?", "did it work?"): answered in words, from the conversation,
/// without choosing a command (docs/talk-design.md §6).
pub fn question_about_answer(text: &str) -> bool {
    const ASKING: [&str; 11] = [
        "is", "are", "was", "were", "does", "did", "do", "has", "have", "can", "could",
    ];
    let first = text
        .split(|c: char| !c.is_ascii_alphabetic())
        .find(|w| !w.is_empty())
        .unwrap_or("");
    follow_up(text) && ASKING.iter().any(|a| a.eq_ignore_ascii_case(first))
}

/// "remember that X", "remember X", "note that X", "don't forget X": the
/// fact to keep (docs/facts-design.md), or `None`.
pub fn fact_to_remember(text: &str) -> Option<&str> {
    let t = text.trim();
    for p in [
        "please remember that ",
        "please remember ",
        "remember that ",
        "remember ",
        "note that ",
        "don't forget that ",
        "dont forget that ",
        "don't forget ",
        "dont forget ",
    ] {
        if t.len() > p.len() && t.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes()) {
            let rest = t[p.len()..].trim();
            return (rest.len() > 2).then_some(rest);
        }
    }
    None
}

/// "summarise X", "summarize the file X", "sum up X": the file to
/// summarise (docs/summary-design.md).
pub fn file_to_summarise(text: &str) -> Option<&str> {
    let t = text.trim().trim_end_matches(['?', '.', '!']);
    for p in [
        "please summarise ",
        "please summarize ",
        "can you summarise ",
        "can you summarize ",
        "give me a summary of ",
        "summary of ",
        "summarise ",
        "summarize ",
        "sum up ",
    ] {
        if t.len() > p.len() && t.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes()) {
            let mut rest = t[p.len()..].trim();
            for lead in ["the file ", "file ", "the ", "my "] {
                if rest.len() > lead.len()
                    && rest.as_bytes()[..lead.len()].eq_ignore_ascii_case(lead.as_bytes())
                {
                    rest = rest[lead.len()..].trim();
                }
            }
            return (!rest.is_empty()).then_some(rest);
        }
    }
    None
}

/// "save that", "save the summary", "save it as a note": keep the last
/// summary in a file (docs/save-design.md).
pub fn wants_saved(text: &str) -> bool {
    let mut t = text.trim().trim_end_matches(['?', '.', '!']);
    if t.len() > 7 && t.as_bytes()[..7].eq_ignore_ascii_case(b"please ") {
        t = t[7..].trim_start();
    }
    [
        "save that",
        "save it",
        "save this",
        "save the summary",
        "save that summary",
        "keep that as a note",
        "make a note of that",
    ]
    .iter()
    .any(|q| {
        t.len() >= q.len()
            && t.as_bytes()[..q.len()].eq_ignore_ascii_case(q.as_bytes())
            && (t.len() == q.len() || t.as_bytes()[q.len()] == b' ')
    })
}

/// The summary of `path` is saved beside it as `<this>-summary.txt`:
/// `path` without its extension.
pub fn summary_stem(path: &str) -> &str {
    let name_at = path.rfind('/').map_or(0, |i| i + 1);
    match path[name_at..].rfind('.') {
        Some(dot) if dot > 0 => &path[..name_at + dot],
        _ => path,
    }
}

/// "forget X", "forget that X", "forget about X": what to forget.
pub fn fact_to_forget(text: &str) -> Option<&str> {
    let t = text.trim();
    for p in ["forget about ", "forget that ", "forget "] {
        if t.len() > p.len() && t.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes()) {
            let rest = t[p.len()..].trim();
            return (!rest.is_empty()).then_some(rest);
        }
    }
    None
}

/// "what do you remember?", "what do you know about me?".
pub fn asks_what_remembered(text: &str) -> bool {
    [
        "what do you remember",
        "what do you know about me",
        "what have i told you",
        "what did i tell you",
    ]
    .iter()
    .any(|q| contains_ignoring_case(text, q))
}

/// A question about what Bella herself changed ("what did you do?"):
/// answered from the broker's audit file (docs/audit-design.md).
pub fn asks_what_bella_did(text: &str) -> bool {
    [
        "what did you do",
        "what have you done",
        "what did you change",
        "what have you changed",
        "what changes did you make",
        "show your history",
        "show me your history",
        "show the audit",
    ]
    .iter()
    .any(|q| contains_ignoring_case(text, q))
}

/// A question about what the user is doing ("what am I working on?",
/// "what's open?"): answered with what is on the screen
/// (docs/context-design.md).
pub fn asks_about_activity(text: &str) -> bool {
    [
        "what am i working on",
        "what was i working on",
        "what am i doing",
        "what was i doing",
        "what's open",
        "whats open",
        "what is open",
        "which windows",
        "what windows",
        "what did i just open",
        "what did i open",
    ]
    .iter()
    .any(|q| contains_ignoring_case(text, q))
}

/// Whether `hay` contains `needle`, ASCII letters compared without case.
fn contains_ignoring_case(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_commands() {
        for t in [
            "search pancake",
            "search pancake recipe",
            "list Music",
            "list",
            "inspect Documents/notes.txt",
            "create Projects/2026",
            "undo",
            "read Documents/notes.txt",
            "open Music",
            "launch Notes",
            "trash Documents/old",
            "info",
            "move Documents/notes.txt to Music",
        ] {
            assert!(typed_command(t), "{t}");
        }
    }

    #[test]
    fn sentences_are_not_commands() {
        for t in [
            "search for my music and tell me the path",
            "search for my music",
            "list my documents",
            "create a folder called Work",
            "undo that",
            "move my notes to the archive",
            "where is my music?",
            "find my pancake recipe",
        ] {
            assert!(!typed_command(t), "{t}");
        }
    }

    #[test]
    fn follow_ups() {
        for t in [
            "is that it?",
            "what about Photos?",
            "open it",
            "anything else",
            "show me more",
            "list them",
        ] {
            assert!(follow_up(t), "{t}");
        }
        for t in [
            "where is my music?",
            "Find my pancake recipe",
            "list Music",
            "Tell me a joke",
            "items in the attic",
        ] {
            assert!(!follow_up(t), "{t}");
        }
    }

    #[test]
    fn questions_about_the_answer() {
        for t in [
            "is that it?",
            "Are there more?",
            "did that work?",
            "is that all of them",
        ] {
            assert!(question_about_answer(t), "{t}");
        }
        for t in [
            "what is in it?",
            "open it",
            "is my music here",
            "Find my pancake recipe",
        ] {
            assert!(!question_about_answer(t), "{t}");
        }
    }

    #[test]
    fn facts() {
        assert_eq!(
            fact_to_remember("Remember that my dentist is on Friday"),
            Some("my dentist is on Friday")
        );
        assert_eq!(
            fact_to_remember("note that Zara likes carrots"),
            Some("Zara likes carrots")
        );
        assert_eq!(fact_to_remember("remember"), None);
        assert_eq!(fact_to_remember("do you remember my music?"), None);
        assert_eq!(
            fact_to_forget("forget about my dentist"),
            Some("my dentist")
        );
        assert_eq!(fact_to_forget("forget"), None);
        assert!(asks_what_remembered("What do you remember?"));
        assert!(!asks_what_remembered("where is my music"));
    }

    #[test]
    fn saved() {
        for t in [
            "save that",
            "Save it as a note.",
            "please save the summary",
            "make a note of that",
        ] {
            assert!(wants_saved(t), "{t}");
        }
        for t in ["saved games", "where is save.txt", "summarise notes.txt"] {
            assert!(!wants_saved(t), "{t}");
        }
        assert_eq!(summary_stem("Documents/notes.txt"), "Documents/notes");
        assert_eq!(summary_stem("todo"), "todo");
        assert_eq!(summary_stem("a/.hidden"), "a/.hidden");
        assert_eq!(summary_stem("v1.2/notes"), "v1.2/notes");
    }

    #[test]
    fn summarise() {
        for (t, want) in [
            ("summarise Documents/notes.txt", "Documents/notes.txt"),
            ("Summarize the file notes.txt?", "notes.txt"),
            ("can you summarise my shopping list", "shopping list"),
            ("give me a summary of zoo.rs", "zoo.rs"),
        ] {
            assert_eq!(file_to_summarise(t), Some(want), "{t}");
        }
        for t in ["summarise", "read notes.txt", "what is a summary"] {
            assert_eq!(file_to_summarise(t), None, "{t}");
        }
    }

    #[test]
    fn bella_did() {
        for t in [
            "What did you do?",
            "what have you changed",
            "show your history",
        ] {
            assert!(asks_what_bella_did(t), "{t}");
        }
        for t in [
            "what did I just open",
            "what do you remember",
            "what am I doing",
        ] {
            assert!(!asks_what_bella_did(t), "{t}");
        }
    }

    #[test]
    fn activity() {
        for t in [
            "What am I working on?",
            "whats open",
            "Which windows are open?",
            "what did I just open",
        ] {
            assert!(asks_about_activity(t), "{t}");
        }
        for t in ["open Music", "where is my music", "what is in Music"] {
            assert!(!asks_about_activity(t), "{t}");
        }
    }

    #[test]
    fn small_talk_is_recognised() {
        for t in [
            "whats your name",
            "What's your name?",
            "hello",
            "Hi Bella",
            "thanks!",
            "What can you do?",
            "who made you",
            "how are you today",
        ] {
            assert!(small_talk(t), "{t}");
        }
        for t in [
            "where is my music?",
            "Find my pancake recipe",
            "list Music",
            "Tell me a joke",
            "this is high",
            "hiking photos",
        ] {
            assert!(!small_talk(t), "{t}");
        }
    }
}
