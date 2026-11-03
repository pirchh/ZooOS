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
