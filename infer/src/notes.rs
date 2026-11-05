//! The edits Bella asks Notes for (docs/registry-design.md §4, docs/targets-design.md): take a
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

/// What an edit did: the new text and a sentence; or, with more than one
/// matching line and no target, those lines to choose from
/// (docs/targets-design.md); or nothing matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Done { text: String, said: String },
    Choose(Vec<String>),
    NotFound,
}

/// A line's closing mark ("." or "!"), kept when it is edited.
fn split_mark(line: &str) -> (&str, &str) {
    let t = line.trim_end();
    if t.ends_with(['.', '!']) {
        (&t[..t.len() - 1], &t[t.len() - 1..])
    } else {
        (t, "")
    }
}

/// The lines to edit: those `hit` accepts, narrowed to the one equal to
/// `target` when given. One: `Ok(index)`; else the outcome.
fn pick(lines: &[&str], hit: impl Fn(&str) -> bool, target: Option<&str>) -> Result<usize, Edit> {
    let mut found: Vec<usize> = (0..lines.len()).filter(|&k| hit(lines[k])).collect();
    if let Some(t) = target {
        found.retain(|&k| lines[k].trim() == t.trim());
    }
    match found.as_slice() {
        [] => Err(Edit::NotFound),
        [k] => Ok(*k),
        many => Err(Edit::Choose(
            many.iter()
                .map(|&k| String::from(lines[k].trim()))
                .collect(),
        )),
    }
}

/// `text` with `what` taken out:
///
/// 1. a line that is `what` (same words, any case or punctuation) is
///    removed;
/// 2. an item of a list line ("flowers, egg and milk") that is `what` is
///    taken off it ("flowers and egg"); the line goes when no item is left.
///
/// More than one matching line and no `target` (the line's text): those
/// lines to choose from, nothing changed. A line's closing mark is kept.
pub fn remove(text: &str, what: &str, target: Option<&str>) -> Edit {
    let want = words(what);
    if want.is_empty() {
        return Edit::NotFound;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let item_of = |line: &str| -> Option<usize> {
        let its = items(split_mark(line).0);
        if its.len() < 2 {
            return None;
        }
        its.iter().position(|i| words(i) == want)
    };
    let k = match pick(&lines, |l| words(l) == want || item_of(l).is_some(), target) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let line = lines[k];
    let mut out: Vec<String> = lines.iter().map(|l| String::from(*l)).collect();
    if words(line) == want {
        out.remove(k);
        return Edit::Done {
            text: out.join(
                "
",
            ),
            said: format!("Removed \"{}\" from", line.trim()),
        };
    }
    let (body, mark) = split_mark(line);
    let its = items(body);
    let j = item_of(line).unwrap_or(0);
    let left: Vec<&str> = its
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != j)
        .map(|(_, s)| *s)
        .collect();
    out[k] = format!("{}{mark}", join_items(&left));
    Edit::Done {
        text: out.join(
            "
",
        ),
        said: format!("Took \"{}\" off \"{}\" in", its[j], line.trim()),
    }
}

/// Where `old` is in `line` as whole words (any case), if it is.
fn whole_word_at(line: &str, old: &str) -> Option<usize> {
    let lower = line.to_lowercase();
    let needle = old.to_lowercase();
    if lower.len() != line.len() {
        return None;
    }
    let is_word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric());
    let mut from = 0;
    while let Some(i) = lower[from..].find(&needle).map(|i| i + from) {
        let stop = i + needle.len();
        if !is_word(line[..i].chars().last()) && !is_word(line[stop..].chars().next()) {
            return Some(i);
        }
        from = stop;
    }
    None
}

/// `text` with `old` (whole words, any case) changed to `new` in the one
/// line that has it; more than one such line and no `target`: those lines
/// to choose from.
pub fn change(text: &str, old: &str, new: &str, target: Option<&str>) -> Edit {
    let (old, new) = (old.trim(), new.trim());
    if old.is_empty() || new.is_empty() {
        return Edit::NotFound;
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let k = match pick(&lines, |l| whole_word_at(l, old).is_some(), target) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let line = lines[k];
    let i = whole_word_at(line, old).unwrap_or(0);
    let was = &line[i..i + old.len()];
    let mut out: Vec<String> = lines.iter().map(|l| String::from(*l)).collect();
    out[k] = format!("{}{}{}", &line[..i], new, &line[i + old.len()..]);
    Edit::Done {
        text: out.join(
            "
",
        ),
        said: format!("Changed \"{was}\" to \"{new}\" in"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "Shopping
flowers, egg and milk
call the vet
";

    fn done(e: Edit) -> (String, String) {
        match e {
            Edit::Done { text, said } => (text, said),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn removes_a_whole_line() {
        let (t, said) = done(remove(NOTE, "Flowers, egg, and milk", None));
        assert_eq!(
            t,
            "Shopping
call the vet
"
        );
        assert_eq!(said, "Removed \"flowers, egg and milk\" from");
    }

    #[test]
    fn takes_an_item_off_a_list() {
        let (t, said) = done(remove(NOTE, "milk", None));
        assert_eq!(
            t,
            "Shopping
flowers and egg
call the vet
"
        );
        assert_eq!(said, "Took \"milk\" off \"flowers, egg and milk\" in");
        assert_eq!(done(remove("a and b", "a", None)).0, "b");
        // The closing mark stays.
        assert_eq!(
            done(remove(
                "Shopping: flour, eggs, milk.
",
                "milk",
                None
            ))
            .0,
            "Shopping: flour and eggs.
"
        );
    }

    #[test]
    fn two_matches_ask_which() {
        let note = "Shopping: flour, eggs, milk.
flowers, bread and milk
";
        assert_eq!(
            remove(note, "milk", None),
            Edit::Choose(alloc::vec![
                String::from("Shopping: flour, eggs, milk."),
                String::from("flowers, bread and milk")
            ])
        );
        let (t, said) = done(remove(note, "milk", Some("flowers, bread and milk")));
        assert_eq!(
            t,
            "Shopping: flour, eggs, milk.
flowers and bread
"
        );
        assert_eq!(said, "Took \"milk\" off \"flowers, bread and milk\" in");
        assert_eq!(remove(note, "milk", Some("no such line")), Edit::NotFound);
        assert!(matches!(
            change(
                "egg one
egg two",
                "egg",
                "x",
                None
            ),
            Edit::Choose(_)
        ));
        assert_eq!(
            done(change(
                "egg one
egg two",
                "egg",
                "x",
                Some("egg two")
            ))
            .0,
            "egg one
x two"
        );
    }

    #[test]
    fn nothing_matches() {
        assert_eq!(remove(NOTE, "bread", None), Edit::NotFound);
        assert_eq!(remove(NOTE, "the", None), Edit::NotFound);
        assert_eq!(remove(NOTE, "", None), Edit::NotFound);
        assert_eq!(change("eggplant", "egg", "x", None), Edit::NotFound);
    }

    #[test]
    fn changes_whole_words() {
        let (t, said) = done(change(NOTE, "Egg", "bread", None));
        assert_eq!(
            t,
            "Shopping
flowers, bread and milk
call the vet
"
        );
        assert_eq!(said, "Changed \"egg\" to \"bread\" in");
    }
}
