//! Requests of several steps (docs/multi-step-design.md): "open Notes and
//! Files, then minimize Files" becomes steps Bella runs one after the
//! other, each chosen on its own.

use alloc::string::String;
use alloc::vec::Vec;

/// Words that start a step of their own after "and": "open Notes and
/// maximize it" is two steps, "split Notes and Files" one
/// (docs/multi-step-design.md).
const STEP_VERBS: [&str; 30] = [
    "open",
    "close",
    "quit",
    "launch",
    "start",
    "maximize",
    "maximise",
    "minimize",
    "minimise",
    "hide",
    "show",
    "split",
    "tile",
    "put",
    "find",
    "search",
    "look",
    "list",
    "read",
    "move",
    "rename",
    "delete",
    "trash",
    "create",
    "make",
    "summarise",
    "summarize",
    "save",
    "then",
    "also",
];

/// The steps of a request of several ("open Notes and Files, then
/// minimize Files"): split at "then", "and then", ";" and sentence ends,
/// and at "and" or a comma when the next word starts a step
/// (`STEP_VERBS`). One step: an empty list. A leading "hey Bella," is
/// dropped. At most `MAX_STEPS` (docs/multi-step-design.md).
pub fn split_steps(text: &str) -> Vec<String> {
    let mut t = text.trim();
    for lead in [
        "hey bella,",
        "hey bella",
        "bella,",
        "ok bella,",
        "okay bella,",
    ] {
        if t.len() >= lead.len() && t[..lead.len()].eq_ignore_ascii_case(lead) {
            t = t[lead.len()..].trim_start();
        }
    }
    // Words, keeping which separator came before each.
    let mut steps: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut words = t.split_whitespace().peekable();
    while let Some(w) = words.next() {
        let bare = w.trim_end_matches([',', ';', '.']);
        let ended = w.len() != bare.len() && !w.ends_with("..");
        let lower = bare.to_ascii_lowercase();
        let next_is_verb = |n: Option<&&str>| {
            n.map(|n| n.trim_end_matches([',', ';', '.']).to_ascii_lowercase())
                .is_some_and(|n| STEP_VERBS.contains(&n.as_str()))
        };
        if lower == "then" && !cur.is_empty() {
            steps.push(core::mem::take(&mut cur));
            continue;
        }
        if lower == "and" && !cur.is_empty() && next_is_verb(words.peek()) {
            steps.push(core::mem::take(&mut cur));
            continue;
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(bare);
        // "Notes, then", "Notes; maximize", "Notes. Then": a boundary when
        // a step word follows (a comma in a list of apps is not one).
        if ended && (w.ends_with(';') || w.ends_with('.') || next_is_verb(words.peek())) {
            steps.push(core::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        steps.push(cur);
    }
    // "then" and "also" at a step's start are only joins.
    for s in steps.iter_mut() {
        for lead in ["then ", "also ", "and "] {
            if s.len() > lead.len() && s[..lead.len()].eq_ignore_ascii_case(lead) {
                *s = String::from(&s[lead.len()..]);
            }
        }
    }
    steps.retain(|s| !s.trim().is_empty());
    if steps.len() < 2 || steps.len() > MAX_STEPS {
        return Vec::new();
    }
    steps
}

/// Most steps in one request.
pub const MAX_STEPS: usize = 5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps() {
        let s = |t: &str| split_steps(t);
        assert_eq!(
            s("open Notes and maximize it"),
            ["open Notes", "maximize it"]
        );
        assert_eq!(
            s("open Notes and Files, then minimize Files"),
            ["open Notes and Files", "minimize Files"]
        );
        assert_eq!(
            s("hey bella open notes then put it on the left"),
            ["open notes", "put it on the left"]
        );
        assert_eq!(
            s("find my pancake recipe and then open it"),
            ["find my pancake recipe", "open it"]
        );
        assert_eq!(
            s("close Files; maximize Notes"),
            ["close Files", "maximize Notes"]
        );
        // One step.
        assert!(s("split Notes and Files").is_empty());
        assert!(s("put notes on the left and the assistant on the right").is_empty());
        assert!(s("search for flour and sugar").is_empty());
        assert!(s("split the screen with the files, notes and network apps").is_empty());
        assert!(s("open notes").is_empty());
    }
}
