//! The edits Bella asks Notes for (docs/registry-design.md §4): take a
//! line, or one item of a list line, out of a note; change words in it.
//! Pure text in, text out, so they are tested here and Notes only applies
//! them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// `s`'s words, lower case, letters and digits only.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// A list line's items: split at commas and at the word "and" (also "&").
fn items(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for part in line.split([',', ';']) {
        let mut rest = part;
        loop {
            let lower = rest.to_ascii_lowercase();
            let cut = [" and ", " & "]
                .iter()
                .filter_map(|s| lower.find(s).map(|i| (i, s.len())))
                .min();
            match cut {
                Some((i, n)) => {
                    out.push(rest[..i].trim());
                    rest = &rest[i + n..];
                }
                None => {
                    out.push(rest.trim());
                    break;
                }
            }
        }
    }
    out.retain(|i| !i.is_empty());
    out
}

/// Items as one line: "a", "a and b", "a, b and c".
fn join_items(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => String::from(*one),
        [rest @ .., last] => format!("{} and {}", rest.join(", "), last),
    }
}

/// `text` with `what` taken out, and a sentence saying what was done:
///
/// 1. a line that is `what` (same words, any case or punctuation) is
///    removed;
/// 2. else an item of a list line ("flowers, egg and milk") that is `what`
///    is taken off it ("flowers and egg"), and the line removed when no
///    item is left.
///
/// The newest match wins (the last line that matches): what was just
/// written is likelier meant than an old line with the same words. A
/// line's closing mark ("." or "!") is kept. `None` when nothing matches.
pub fn remove(text: &str, what: &str) -> Option<(String, String)> {
    let want = words(what);
    if want.is_empty() {
        return None;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    if let Some(k) = lines.iter().rposition(|l| words(l) == want) {
        let mut out: Vec<&str> = lines.clone();
        let gone = out.remove(k).trim();
        return Some((out.join("\n"), format!("Removed \"{gone}\" from")));
    }
    for (k, line) in lines.iter().enumerate().rev() {
        let trimmed = line.trim_end();
        let mark = if trimmed.ends_with(['.', '!']) {
            &trimmed[trimmed.len() - 1..]
        } else {
            ""
        };
        let its = items(&trimmed[..trimmed.len() - mark.len()]);
        if its.len() < 2 {
            continue;
        }
        if let Some(j) = its.iter().position(|i| words(i) == want) {
            let gone = its[j];
            let left: Vec<&str> = its
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != j)
                .map(|(_, s)| *s)
                .collect();
            let new_line = format!("{}{mark}", join_items(&left));
            let mut out: Vec<String> = lines.iter().map(|l| String::from(*l)).collect();
            out[k] = new_line.clone();
            return Some((
                out.join("\n"),
                format!("Took \"{gone}\" off \"{}\" in", line.trim()),
            ));
        }
    }
    None
}

/// `text` with the last `old` (whole words, any case; the newest, as for
/// `remove`) changed to `new`, and a sentence saying so; `None` when `old`
/// is not there.
pub fn change(text: &str, old: &str, new: &str) -> Option<(String, String)> {
    let old = old.trim();
    let new = new.trim();
    if old.is_empty() || new.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let needle = old.to_lowercase();
    let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
    let mut end = lower.len();
    while let Some(i) = lower[..end].rfind(&needle) {
        let stop = i + needle.len();
        // Whole words only: "egg" is not changed inside "eggplant". Byte
        // positions match because `to_lowercase` kept ASCII lengths; any
        // other text is compared as is.
        if lower.len() == text.len()
            && !is_word(text[..i].chars().last())
            && !is_word(text[stop..].chars().next())
        {
            let out = format!("{}{}{}", &text[..i], new, &text[stop..]);
            return Some((
                out,
                format!("Changed \"{}\" to \"{new}\" in", &text[i..stop]),
            ));
        }
        end = i;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "Shopping\nflowers, egg and milk\ncall the vet\n";

    #[test]
    fn removes_a_whole_line() {
        let (t, said) = remove(NOTE, "Flowers, egg, and milk").unwrap();
        assert_eq!(t, "Shopping\ncall the vet\n");
        assert_eq!(said, "Removed \"flowers, egg and milk\" from");
        assert_eq!(
            remove(NOTE, "call the vet").unwrap().0,
            "Shopping\nflowers, egg and milk\n"
        );
    }

    #[test]
    fn takes_an_item_off_a_list() {
        let (t, said) = remove(NOTE, "milk").unwrap();
        assert_eq!(t, "Shopping\nflowers and egg\ncall the vet\n");
        assert_eq!(said, "Took \"milk\" off \"flowers, egg and milk\" in");
        assert_eq!(
            remove(NOTE, "flowers").unwrap().0,
            "Shopping\negg and milk\ncall the vet\n"
        );
        assert_eq!(remove("a and b", "a").unwrap().0, "b");
    }

    #[test]
    fn the_newest_match_and_the_closing_mark() {
        let note = "Shopping: flour, eggs, milk.
flowers, bread and milk
";
        let (t, said) = remove(note, "milk").unwrap();
        assert_eq!(
            t,
            "Shopping: flour, eggs, milk.
flowers and bread
"
        );
        assert_eq!(said, "Took \"milk\" off \"flowers, bread and milk\" in");
        let (t, said) = remove(
            "Shopping: flour, eggs, milk.
",
            "milk",
        )
        .unwrap();
        assert_eq!(
            t,
            "Shopping: flour and eggs.
"
        );
        assert_eq!(
            said,
            "Took \"milk\" off \"Shopping: flour, eggs, milk.\" in"
        );
        assert_eq!(
            change(
                "egg one
egg two",
                "egg",
                "x"
            )
            .unwrap()
            .0,
            "egg one
x two"
        );
    }

    #[test]
    fn nothing_matches() {
        assert!(remove(NOTE, "bread").is_none());
        assert!(remove(NOTE, "the").is_none());
        assert!(remove(NOTE, "").is_none());
    }

    #[test]
    fn changes_whole_words() {
        let (t, said) = change(NOTE, "Egg", "bread").unwrap();
        assert_eq!(t, "Shopping\nflowers, bread and milk\ncall the vet\n");
        assert_eq!(said, "Changed \"egg\" to \"bread\" in");
        assert!(change("eggplant", "egg", "x").is_none());
        assert!(change(NOTE, "bread", "x").is_none());
    }
}
