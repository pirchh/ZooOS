//! Templates from Bella's memory of confirmed phrasings
//! (docs/templates-design.md): a remembered request and its command, used
//! for a request that differs from it only in the app it names.

use alloc::string::String;
use alloc::vec::Vec;

/// Words that change nothing in a request ("make *my* notes big").
const FILLER: [&str; 6] = ["my", "the", "a", "please", "now", "window"];

/// The command for `request` by the pattern of a remembered pair: when
/// `request` is `remembered` word for word with one app swapped for
/// another ("make files big" after "make notes big" -> `maximize Notes`),
/// the command with that app swapped too (`maximize Files`). `None` unless
/// each names exactly one app, the apps differ, and the command names
/// the remembered one.
pub fn by_pattern(remembered: &str, command: &str, request: &str) -> Option<String> {
    let (a, b) = (
        crate::assist::apps_named(remembered, false),
        crate::assist::apps_named(request, false),
    );
    let ([old], [new]) = (a.as_slice(), b.as_slice()) else {
        return None;
    };
    // A routine is named, not a pattern (docs/routines-design.md).
    if old == new || command.starts_with("steps ") {
        return None;
    }
    if shape(remembered, old) != shape(request, new) {
        return None;
    }
    let mut out = String::new();
    let mut named = false;
    for (k, w) in command.split(' ').enumerate() {
        if k > 0 {
            out.push(' ');
        }
        if names_app(w, old) {
            named = true;
            out.push_str(new);
        } else {
            out.push_str(w);
        }
    }
    named.then_some(out)
}

/// `text`'s words in order, lower case, filler left out, with `app` as
/// one placeholder.
fn shape(text: &str, app: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| {
            if names_app(w, app) {
                String::from("\u{1}")
            } else {
                w.to_ascii_lowercase()
            }
        })
        .filter(|w| !FILLER.contains(&w.as_str()))
        .collect()
}

/// Whether word `w` names `app` (Finder is Files).
fn names_app(w: &str, app: &str) -> bool {
    w.eq_ignore_ascii_case(app) || (app == "Files" && w.eq_ignore_ascii_case("finder"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swapped_app() {
        assert_eq!(
            by_pattern("make notes big", "maximize Notes", "make my files big").as_deref(),
            Some("maximize Files")
        );
        assert_eq!(
            by_pattern("put notes away", "minimize Notes", "put network away").as_deref(),
            Some("minimize Network")
        );
        // Other words differ: not the pattern.
        assert_eq!(
            by_pattern("make notes big", "maximize Notes", "make files small"),
            None
        );
        // The same app, no app, or two apps: nothing to swap.
        assert_eq!(
            by_pattern("make notes big", "maximize Notes", "make notes big"),
            None
        );
        assert_eq!(
            by_pattern("notes mode", "steps open notes | minimize it", "files mode"),
            None
        );
        assert_eq!(
            by_pattern("tidy up", "split Files and Notes", "tidy up"),
            None
        );
        // The command doesn't name the app: not a pattern over it.
        assert_eq!(
            by_pattern("show me notes", "layout v(...)", "show me files"),
            None
        );
    }
}
