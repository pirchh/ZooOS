//! How Bella routes a line typed into her window (docs/talk-design.md): a
//! typed command, small talk answered in words, or a request for the
//! model. Pure text rules, kept here so the host can test them.

/// The system's own commands the user can type (docs/access-design.md
/// adds `read`, `open`, `move`, `trash`, `launch` and `info`;
/// docs/window-actions-design.md `maximize`, `minimize` and `close`). App
/// actions' words come from the registry (`typed_command_with`).
pub const COMMANDS: [&str; 17] = [
    "search", "list", "inspect", "create", "undo", "read", "open", "move", "trash", "launch",
    "info", "maximize", "minimize", "close", "split", "show", "layout",
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
    typed_command_with(text, |_| false)
}

/// `typed_command`, with `app_word` saying which words are registered app
/// actions (docs/registry-design.md). Their text is free, "and" and "to"
/// included ("write flowers, egg and milk", "change egg to bread"); only
/// a sentence word right after the command word makes it a request
/// ("write me a poem", "remove my old notes").
pub fn typed_command_with(text: &str, app_word: impl Fn(&str) -> bool) -> bool {
    let mut words = text.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if app_word(first) {
        return words
            .next()
            .is_some_and(|w| !SENTENCE_WORDS.iter().any(|s| s.eq_ignore_ascii_case(w)));
    }
    if !COMMANDS.contains(&first) {
        return false;
    }
    // `show <app> on monitor N` (docs/monitors-design.md); "show Documents"
    // stays a request for the model.
    if first == "show"
        && !text
            .split_whitespace()
            .any(|w| w.eq_ignore_ascii_case("monitor"))
    {
        return false;
    }
    if first == "undo" || first == "info" || (first == "split" && text.trim() == "split") {
        return words.next().is_none();
    }
    // `move <path> to <path>` and `split <app> and <app>`: there, "to"
    // and "and" are part of the command.
    words.all(|w| {
        (first == "move" && w.eq_ignore_ascii_case("to"))
            || (matches!(
                first,
                "split" | "launch" | "maximize" | "minimize" | "close"
            ) && w.eq_ignore_ascii_case("and"))
            || !SENTENCE_WORDS.iter().any(|s| s.eq_ignore_ascii_case(w))
    })
}

/// Greetings before her name, including how speech to text writes "hey"
/// ("they", "hay": the owner's "hey Bella" came out "They bellow").
const GREETINGS: [&str; 9] = [
    "hey", "hi", "ok", "okay", "hello", "they", "hay", "hei", "eh",
];
/// Words people say before a request without meaning anything by them.
const FILLERS: [&str; 12] = [
    "um", "uh", "erm", "er", "so", "and", "well", "alright", "right", "now", "oh", "ah",
];
/// Words near "bella" that are words of their own, never her name.
const NOT_HER_NAME: [&str; 8] = [
    "hello", "below", "bells", "belly", "fella", "fellow", "yellow", "jelly",
];

/// Where `text` speaks to Bella: whether it starts with an optional
/// greeting, then her name, and what follows (the request, perhaps empty).
/// After a greeting, a word up to two letters from "Bella" counts ("hey
/// Bela", "they bellow"); without one the name must be exact.
fn after_name(text: &str) -> Option<&str> {
    let not_word = |c: char| !c.is_ascii_alphanumeric() && c != '\'';
    let mut rest = text.trim_start_matches(not_word);
    let word = |t: &str| -> usize { t.find(not_word).unwrap_or(t.len()) };
    // Up to three words of filler before the greeting or her name ("um,
    // so, hey Bella"): spoken requests seldom start cleanly.
    for _ in 0..3 {
        let w = word(rest);
        if !FILLERS.iter().any(|f| rest[..w].eq_ignore_ascii_case(f)) {
            break;
        }
        rest = rest[w..].trim_start_matches(not_word);
    }
    let w = word(rest);
    let first = &rest[..w];
    // Speech to text writes "hey" many ways ("they", "pay", "hay"): a 3–4
    // letter word that sounds like it (one letter off, or ending "ay" or
    // "ey") counts, since her name must follow.
    let ends = |e: &str| first.len() >= 2 && first[first.len() - 2..].eq_ignore_ascii_case(e);
    let greeted = GREETINGS.iter().any(|g| first.eq_ignore_ascii_case(g))
        || ((3..=4).contains(&first.len())
            && (distance(first, "hey") <= 1 || ends("ay") || ends("ey")));
    if greeted {
        rest = rest[w..].trim_start_matches(not_word);
        // "okay so Bella": filler after the greeting too.
        for _ in 0..2 {
            let w = word(rest);
            if !FILLERS.iter().any(|f| rest[..w].eq_ignore_ascii_case(f)) {
                break;
            }
            rest = rest[w..].trim_start_matches(not_word);
        }
    }
    let w = word(rest);
    let name = &rest[..w];
    let near =
        greeted && near_bella(name) && !NOT_HER_NAME.iter().any(|n| name.eq_ignore_ascii_case(n));
    if !(name.eq_ignore_ascii_case("bella") || near) {
        return None;
    }
    Some(rest[w..].trim_start_matches(not_word).trim_end())
}

/// The request after Bella's name when `text` speaks to her: an optional
/// greeting ("hey", "hi", "ok", "okay", "hello"; "they", "hay" as speech
/// to text writes "hey"), then "Bella", then the request, with any
/// punctuation between ("Hey, Bella. Open notes", "OK Bella: split").
/// `None` when it does not start that way or nothing follows the name
/// (docs/listening-design.md).
pub fn addressed(text: &str) -> Option<&str> {
    after_name(text).filter(|r| !r.is_empty())
}

/// Whether `text` is her name and nothing else ("Hey Bella.", "They
/// bellow,"): someone who pauses after the name, the request to follow.
pub fn name_only(text: &str) -> bool {
    after_name(text).is_some_and(|r| r.is_empty())
}

/// Whether `w` is close to "bella": at most two letters changed, added or
/// removed for a word of 4 to 6 letters ("bellow", "billah"), one for 7
/// ("Bellamy" is someone else).
fn near_bella(w: &str) -> bool {
    (4..=7).contains(&w.len()) && distance(w, "bella") <= if w.len() <= 6 { 2 } else { 1 }
}

/// Letters changed, added or removed between `a` and `b` (ASCII, any
/// case; at most 7 letters each, else a large number).
fn distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() > 7 || b.len() > 7 || !a.is_ascii() || !b.is_ascii() {
        return usize::MAX;
    }
    let mut prev = [0usize; 8];
    for (j, p) in prev.iter_mut().enumerate().take(b.len() + 1) {
        *p = j;
    }
    for (i, &ca) in a.iter().enumerate() {
        let mut cur = [0usize; 8];
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(!ca.eq_ignore_ascii_case(&cb));
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Whether `text` asks *not* to do something ("don't delete the milk
/// entry", "do not close notes", "never mind"): Bella does nothing
/// (docs/targets-design.md).
pub fn negated(text: &str) -> bool {
    let t = text
        .trim_start()
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    let starts = |p: &str| t.len() >= p.len() && t[..p.len()].eq_ignore_ascii_case(p);
    [
        "don't ",
        "dont ",
        "do not ",
        "never ",
        "please don't ",
        "please do not ",
    ]
    .iter()
    .any(|p| starts(p))
        || t.eq_ignore_ascii_case("never mind")
        || t.eq_ignore_ascii_case("nevermind")
}

/// Which of `choices` a spoken answer picks: "the first", "two", "the
/// last", or the one line sharing a word of three letters or more with the
/// answer that no other line has.
pub fn spoken_choice(answer: &str, choices: &[&str]) -> Option<usize> {
    // "first", "second", "third", "last" anywhere; a bare number ("two",
    // "2") only as the whole answer ("the one with flowers" is not 1).
    const ORDINALS: [(&str, usize); 3] = [("first", 0), ("second", 1), ("third", 2)];
    const NUMBERS: [(&str, usize); 6] = [
        ("one", 0),
        ("1", 0),
        ("two", 1),
        ("2", 1),
        ("three", 2),
        ("3", 2),
    ];
    let mut count = 0;
    let mut only = "";
    for w in answer
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        count += 1;
        only = w;
        if let Some((_, k)) = ORDINALS.iter().find(|(o, _)| w.eq_ignore_ascii_case(o)) {
            return (*k < choices.len()).then_some(*k);
        }
        if w.eq_ignore_ascii_case("last") && !choices.is_empty() {
            return Some(choices.len() - 1);
        }
    }
    if count == 1
        && let Some((_, k)) = NUMBERS.iter().find(|(n, _)| only.eq_ignore_ascii_case(n))
    {
        return (*k < choices.len()).then_some(*k);
    }
    // A distinctive word.
    let has = |line: &str, w: &str| {
        line.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|x| x.eq_ignore_ascii_case(w))
    };
    let mut pick = None;
    for w in answer
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 3)
    {
        let hits: alloc_free::Hits = alloc_free::hits(choices, |c| has(c, w));
        if hits.count == 1 {
            match pick {
                None => pick = Some(hits.first),
                Some(p) if p != hits.first => return None,
                _ => {}
            }
        }
    }
    pick
}

/// Counting without an allocator (`zoo_abi` has none).
mod alloc_free {
    pub struct Hits {
        pub count: usize,
        pub first: usize,
    }
    pub fn hits(choices: &[&str], f: impl Fn(&str) -> bool) -> Hits {
        let mut h = Hits { count: 0, first: 0 };
        for (k, c) in choices.iter().enumerate() {
            if f(c) {
                if h.count == 0 {
                    h.first = k;
                }
                h.count += 1;
            }
        }
        h
    }
}

/// `text` without a leading "hey Bella" or "Bella" (`addressed`), so a
/// spoken request reads as typed; unchanged otherwise.
pub fn strip_address(text: &str) -> &str {
    addressed(text).unwrap_or(text.trim())
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
    follow_up(text) && ASKING.iter().any(|a| a.eq_ignore_ascii_case(first)) && !polite_request(text)
}

/// "can you split these windows", "could you put them side by side": a
/// request asked politely, not a question about the last answer
/// (docs/multi-step-design.md).
fn polite_request(text: &str) -> bool {
    const ACTIONS: [&str; 26] = [
        "search", "list", "inspect", "create", "read", "open", "move", "trash", "launch",
        "maximize", "maximise", "minimize", "minimise", "close", "split", "tile", "put", "show",
        "hide", "start", "quit", "make", "find", "delete", "rename", "arrange",
    ];
    let mut words = text
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty());
    let (Some(a), Some(b), Some(c)) = (words.next(), words.next(), words.next()) else {
        return false;
    };
    let c = if c.eq_ignore_ascii_case("please") {
        words.next().unwrap_or("")
    } else {
        c
    };
    ["can", "could", "would", "will"]
        .iter()
        .any(|w| w.eq_ignore_ascii_case(a))
        && b.eq_ignore_ascii_case("you")
        && ACTIONS.iter().any(|w| w.eq_ignore_ascii_case(c))
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

/// A question about what the user's own files say ("what do my notes say
/// about the vet?"): answered from lines of those files
/// (docs/lookup-design.md).
pub fn asks_my_files(text: &str) -> bool {
    let about_files = [
        "my notes",
        "my files",
        "my documents",
        "did i write",
        "did i note",
        "have i written",
        "i wrote",
    ]
    .iter()
    .any(|q| contains_ignoring_case(text, q));
    // Asking what they say, not where they are ("where are my files").
    let about_content = [
        "say", "said", "about", "mention", "anything", "what", "when", "who",
    ]
    .iter()
    .any(|q| {
        text.split(|c: char| !c.is_ascii_alphabetic())
            .any(|w| w.eq_ignore_ascii_case(q))
    });
    about_files && about_content && !typed_command(text)
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
    fn polite_requests_are_requests() {
        assert!(!question_about_answer("can you split these 2 windows"));
        assert!(!question_about_answer(
            "could you please put them side by side"
        ));
        assert!(question_about_answer("is that it?"));
        assert!(question_about_answer("can you tell me more about that?"));
    }

    #[test]
    fn address() {
        assert_eq!(
            strip_address("hey bella split these 2 windows"),
            "split these 2 windows"
        );
        assert_eq!(strip_address("Bella, open notes"), "open notes");
        assert_eq!(strip_address("hey bella"), "hey bella");
        assert_eq!(strip_address("bellamy is here"), "bellamy is here");
        assert_eq!(strip_address("open notes"), "open notes");
        assert_eq!(addressed("Hey, Bella. Open notes."), Some("Open notes."));
        assert_eq!(addressed("OK Bella: split"), Some("split"));
        assert_eq!(addressed("hey bella"), None);
        assert_eq!(addressed("Hey Bellamy, open notes"), None);
        assert_eq!(addressed("Put notes on the left"), None);
        assert_eq!(addressed("hey there bella open notes"), None);
        assert_eq!(addressed("Hey Bela, open notes"), Some("open notes"));
        assert_eq!(
            addressed("hey bello maximize notes"),
            Some("maximize notes")
        );
        assert_eq!(addressed("Bela, open notes"), None);
        assert_eq!(addressed("Della, open notes"), None);
        assert_eq!(addressed("hey hello open notes"), None);
        assert_eq!(addressed("Abele maximize my notes."), None);
        // The owner's voice (docs/listening-design.md §7).
        assert!(name_only("They bellow,"));
        assert!(name_only("Hey Bella."));
        assert!(!name_only("Maximize my notes."));
        assert!(!name_only("Hey Bella, open notes"));
        assert_eq!(
            addressed("They bellow, maximize my notes."),
            Some("maximize my notes.")
        );
        assert_eq!(addressed("hey below the line"), None);
        assert_eq!(addressed("they yellow car"), None);
        assert_eq!(addressed("They said hello"), None);
        // Heard in the voice-confirm case (docs/registry-design.md).
        assert_eq!(addressed("Pay billah, remove milk."), Some("remove milk."));
        assert_eq!(addressed("Say hello to everyone"), None);
        assert_eq!(addressed("Pay the bill"), None);
        // Negation and choices (docs/targets-design.md).
        assert!(negated("Don't delete the milk entry"));
        assert!(negated("do not close notes"));
        assert!(negated("never mind"));
        assert!(!negated("delete the milk entry"));
        assert!(!negated("donut shop list"));
        let lines = ["Shopping: flour, eggs, milk.", "flowers, bread and milk"];
        assert_eq!(spoken_choice("the first one", &lines), Some(0));
        assert_eq!(spoken_choice("two", &lines), Some(1));
        assert_eq!(spoken_choice("the last", &lines), Some(1));
        assert_eq!(spoken_choice("the shopping one", &lines), Some(0));
        assert_eq!(spoken_choice("the one with flowers", &lines), Some(1));
        assert_eq!(spoken_choice("milk", &lines), None, "in both");
        assert_eq!(spoken_choice("third", &lines), None);
        // Filler before her name (the owner's demo, docs/listening-design.md §8).
        assert_eq!(
            addressed("Um, so, hey Bella, open notes."),
            Some("open notes.")
        );
        assert_eq!(
            addressed("Okay so Bella what time is it"),
            Some("what time is it")
        );
        assert_eq!(
            addressed("So Bella, what's in my notes?"),
            Some("what's in my notes?")
        );
        assert_eq!(addressed("So I told Bella about it"), None);
    }

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
            "maximize notes",
            "minimize Files",
            "close Network",
            "split notes and bella",
            "split Files",
            "split",
            "split files, notes and network",
            "launch notes and files",
            "show notes on monitor 2",
            "maximize notes on monitor 2",
        ] {
            assert!(typed_command(t), "{t}");
        }
        // In words: a request for the model.
        assert!(!typed_command("maximize my notes please"));
        assert!(!typed_command("close the notes app"));
        assert!(!typed_command("split my screen please"));
        assert!(!typed_command("show Documents"));
        // Registered app actions (docs/registry-design.md).
        let app = |w: &str| ["write", "remove", "change", "browse"].contains(&w);
        for t in [
            "write flowers, egg and milk",
            "remove milk",
            "change egg to bread",
            "browse example.com",
        ] {
            assert!(typed_command_with(t, app), "{t}");
        }
        // A typed or spoken command, not one for the model (the review's
        // "Write 'bread and butter'", which the model alone left as none).
        assert!(typed_command_with("Write 'bread and butter'", |w| w
            .eq_ignore_ascii_case("write")));
        for t in [
            "write me a poem",
            "remove my old notes please",
            "write",
            "change the world",
        ] {
            assert!(!typed_command_with(t, app), "{t}");
        }
        assert!(!typed_command("write flowers"), "not registered here");
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
    fn my_files() {
        for t in [
            "What do my notes say about the vet?",
            "did I write anything about Zara",
            "what's in my files about pancakes",
        ] {
            assert!(asks_my_files(t), "{t}");
        }
        for t in [
            "where are my files",
            "open my notes file",
            "list my documents",
            "what time is it",
        ] {
            assert!(!asks_my_files(t), "{t}");
        }
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
