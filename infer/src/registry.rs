//! The action registry (docs/registry-design.md): what each app can do for
//! Bella, read from its `etc/actions/<program>.actions` file. Bella's
//! commands, the model's prompt lines, how a request's own words fill an
//! action's slots, the question before a delete, and the window server's
//! checks all come from here, so a new app action needs no change to Bella.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// Where the registry files are in the boot image.
pub const DIR: &str = "etc/actions/";
/// The files' ending.
pub const SUFFIX: &str = ".actions";
/// Longest line and file read.
pub const MAX_LINE: usize = 200;
pub const MAX_FILE: usize = 8 * 1024;
/// Longest filled argument (all slots).
pub const MAX_ARG: usize = 400;
/// Between slots in an invocation (`do:<program>:<action>:<a>\x1f<b>`).
pub const SLOT_SEP: char = '\u{1f}';

/// What an action does to the user's things: `Delete` is always asked
/// about first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Read,
    Change,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    /// The program that performs it (the window server's name for it).
    pub program: String,
    /// The action's name in invocations.
    pub name: String,
    /// The model's command word (the name unless `word` says otherwise).
    pub word: String,
    pub about: String,
    pub effect: Effect,
    /// Slot names; `address` is a web address, any other is text.
    pub slots: Vec<String>,
    pub verbs: Vec<String>,
    pub places: Vec<String>,
    pub joins: Vec<String>,
    /// The question before a `Delete`, with `{slot}` filled in.
    pub ask: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    pub actions: Vec<Action>,
}

/// Words before any request that are not part of it.
const COURTESY_FRONT: [&str; 7] = [
    "hey bella",
    "bella",
    "please",
    "can you",
    "could you",
    "would you",
    "will you",
];
const COURTESY_END: [&str; 2] = [" please", " thanks"];

fn list(v: &str) -> Vec<String> {
    v.split('|')
        .map(|s| String::from(s.trim()))
        .filter(|s| !s.is_empty())
        .collect()
}

impl Registry {
    /// Adds the actions of one file. A malformed entry is skipped with a
    /// reason in `skipped`; the rest still load.
    pub fn add_file(&mut self, file: &str, skipped: &mut Vec<String>) {
        let mut program = String::new();
        let mut cur: Option<Action> = None;
        let mut bad: Option<String> = None;
        fn finish(
            cur: &mut Option<Action>,
            bad: &mut Option<String>,
            reg: &mut Registry,
            skipped: &mut Vec<String>,
        ) {
            if let Some(a) = cur.take() {
                let why = bad.take().or_else(|| {
                    if a.about.is_empty() {
                        Some(String::from("no about"))
                    } else if a.slots.is_empty() {
                        Some(String::from("no slots"))
                    } else if a.slots.len() > 1 && a.joins.is_empty() {
                        Some(String::from("two slots and no joins"))
                    } else if a.slots.len() > 2 {
                        Some(String::from("more than two slots"))
                    } else if a.effect == Effect::Delete && a.ask.is_empty() {
                        Some(String::from("a delete with no ask"))
                    } else if reg.find_word(&a.word).is_some() {
                        Some(format!("the word `{}` is taken", a.word))
                    } else {
                        None
                    }
                });
                match why {
                    Some(w) => skipped.push(format!("{}.{}: {}", a.program, a.name, w)),
                    None => reg.actions.push(a),
                }
            }
        }
        for raw in file.lines().take(MAX_FILE / 8) {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if raw.len() > MAX_LINE {
                bad = Some(String::from("a line is too long"));
                continue;
            }
            let (key, value) = line
                .split_once(' ')
                .map_or((line, ""), |(k, v)| (k, v.trim()));
            match key {
                "app" => {
                    finish(&mut cur, &mut bad, self, skipped);
                    program = String::from(value);
                }
                "action" => {
                    finish(&mut cur, &mut bad, self, skipped);
                    if program.is_empty() {
                        skipped.push(format!("?.{value}: no app line before it"));
                        continue;
                    }
                    cur = Some(Action {
                        program: program.clone(),
                        name: String::from(value),
                        word: String::from(value),
                        about: String::new(),
                        effect: Effect::Change,
                        slots: Vec::new(),
                        verbs: Vec::new(),
                        places: Vec::new(),
                        joins: Vec::new(),
                        ask: String::new(),
                    });
                }
                _ => {
                    let Some(a) = cur.as_mut() else {
                        continue;
                    };
                    match key {
                        "word" => a.word = String::from(value),
                        "about" => a.about = String::from(value),
                        "effect" => match value {
                            "read" => a.effect = Effect::Read,
                            "change" => a.effect = Effect::Change,
                            "delete" => a.effect = Effect::Delete,
                            _ => bad = Some(format!("unknown effect `{value}`")),
                        },
                        "slots" => a.slots = value.split_whitespace().map(String::from).collect(),
                        "verbs" => a.verbs = list(value),
                        "places" => a.places = list(value),
                        "joins" => a.joins = list(value),
                        "ask" => a.ask = String::from(value),
                        _ => bad = Some(format!("unknown key `{key}`")),
                    }
                }
            }
        }
        finish(&mut cur, &mut bad, self, skipped);
    }

    pub fn find_word(&self, word: &str) -> Option<&Action> {
        self.actions
            .iter()
            .find(|a| a.word.eq_ignore_ascii_case(word))
    }

    pub fn find(&self, program: &str, name: &str) -> Option<&Action> {
        self.actions
            .iter()
            .find(|a| a.program == program && a.name == name)
    }

    /// An invocation's parts (`<program>:<action>:<slots>`, the text after
    /// `do:`), checked: the action is registered, it has as many slots as
    /// it declares, none empty, and not too long.
    pub fn check<'a>(&self, rest: &'a str) -> Option<(&Action, &'a str)> {
        let (program, rest) = rest.split_once(':')?;
        let (name, arg) = rest.split_once(':').unwrap_or((rest, ""));
        let a = self.find(program, name)?;
        let arg = arg.trim();
        // A request id (`#7`) and a chosen target (`@<line>`) ride along
        // after the slots (docs/targets-design.md).
        let slots = arg.split(SLOT_SEP).filter(|p| !is_extra(p));
        (arg.len() <= MAX_ARG
            && slots.clone().count() == a.slots.len()
            && slots.into_iter().all(|p| !p.trim().is_empty()))
        .then_some((a, arg))
    }
}

/// A part of an invocation's argument that is not a slot: the request id
/// (`#<n>`), a chosen target (`@<text>`), or a titled note (`^<title>`,
/// docs/titled-design.md).
pub fn is_extra(part: &str) -> bool {
    part.starts_with('#') || part.starts_with('@') || part.starts_with('^')
}

/// The titled note an invocation's argument names (`^<title>`).
pub fn note_of(arg: &str) -> Option<&str> {
    arg.split(SLOT_SEP).find_map(|p| p.strip_prefix('^'))
}

/// An invocation's argument split: the slots, the request id, the target.
pub fn split_arg(arg: &str) -> (Vec<&str>, Option<&str>, Option<&str>) {
    let mut slots = Vec::new();
    let (mut id, mut target) = (None, None);
    for p in arg.split(SLOT_SEP) {
        if let Some(i) = p.strip_prefix('#') {
            id = Some(i);
        } else if let Some(t) = p.strip_prefix('@') {
            target = Some(t);
        } else if p.starts_with('^') {
            // The note: `note_of`.
        } else {
            slots.push(p);
        }
    }
    (slots, id, target)
}

/// What an app answers (`did:`), structured (docs/targets-design.md):
/// the status, the request id, a sentence, and for `Choose` the
/// candidates. A plain sentence (older apps) is `Ok` with no id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub status: Status,
    pub id: Option<String>,
    pub said: String,
    pub choices: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Err,
    /// Several targets: which one?
    Choose,
}

impl Outcome {
    /// The text an app sends after `did:`.
    pub fn text(&self) -> String {
        let status = match self.status {
            Status::Ok => "ok",
            Status::Err => "err",
            Status::Choose => "choose",
        };
        let mut t = format!(
            "{status}{SLOT_SEP}#{}{SLOT_SEP}{}",
            self.id.as_deref().unwrap_or(""),
            self.said
        );
        for c in &self.choices {
            t.push(SLOT_SEP);
            t.push_str(c);
        }
        t
    }

    pub fn parse(text: &str) -> Outcome {
        let mut parts = text.split(SLOT_SEP);
        let status = match parts.next() {
            Some("ok") => Status::Ok,
            Some("err") => Status::Err,
            Some("choose") => Status::Choose,
            _ => {
                let st = if text.starts_with("Error") {
                    Status::Err
                } else {
                    Status::Ok
                };
                return Outcome {
                    status: st,
                    id: None,
                    said: String::from(text),
                    choices: Vec::new(),
                };
            }
        };
        let id = parts
            .next()
            .and_then(|p| p.strip_prefix('#'))
            .filter(|i| !i.is_empty())
            .map(String::from);
        let said = String::from(parts.next().unwrap_or(""));
        Outcome {
            status,
            id,
            said,
            choices: parts.map(String::from).collect(),
        }
    }
}

impl Registry {
    /// The command a request names in an action's own words
    /// (docs/regression-design.md): it starts with one of the action's
    /// verbs and ends with one of its places of two words or more ("add
    /// milk to my notes", "delete eggs from my list"), and its slots fill.
    /// `None` when no action, or more than one, reads it that way: the
    /// model chooses then. One-word places ("down", "off") are left out,
    /// so "put it down" stays the model's.
    pub fn phrased(&self, request: &str) -> Option<String> {
        let t = request.trim().trim_end_matches(['.', '!', '?', ',']);
        let lower = t.to_ascii_lowercase();
        let mut found = None;
        for a in &self.actions {
            if a.slots.iter().any(|s| s == "address") {
                continue;
            }
            let verb = a.verbs.iter().any(|v| {
                let v = v.to_ascii_lowercase();
                lower.len() > v.len() && lower.starts_with(&v) && lower[v.len()..].starts_with(' ')
            });
            let place = a.places.iter().any(|p| {
                p.contains(' ') && lower.ends_with(&format!(" {}", p.to_ascii_lowercase()))
            });
            if !(verb && place) {
                continue;
            }
            let slots = a.fill(t);
            if slots.is_empty() {
                continue;
            }
            if found.is_some() {
                return None;
            }
            // As the model's choice: the word, then the slots (joined).
            found = Some(format!("{} {slots}", a.word));
        }
        found
    }
}

impl Action {
    /// The model's prompt line: "change <old> to <new>: change words in
    /// Notes".
    pub fn prompt_line(&self) -> String {
        let mut l = self.word.clone();
        for (k, s) in self.slots.iter().enumerate() {
            if k > 0 {
                l.push(' ');
                l.push_str(self.joins.first().map_or("to", String::as_str));
            }
            l.push_str(" <");
            l.push_str(s);
            l.push('>');
        }
        l.push_str(": ");
        l.push_str(&self.about);
        l
    }

    /// The slots, filled from `request`'s own words, joined by `SLOT_SEP`;
    /// empty when they cannot be filled.
    pub fn fill(&self, request: &str) -> String {
        if self.slots.len() == 1 && self.slots[0] == "address" {
            return crate::assist::address_in(request)
                .map(String::from)
                .unwrap_or_default();
        }
        let t = self.strip(request);
        if self.slots.len() == 1 {
            return clean(&t);
        }
        let lower = t.to_ascii_lowercase();
        let cut = self
            .joins
            .iter()
            .filter_map(|j| {
                let pat = format!(" {} ", j.to_ascii_lowercase());
                lower.find(&pat).map(|i| (i, pat.len()))
            })
            .min();
        let Some((i, n)) = cut else {
            return String::new();
        };
        let (a, b) = (clean(&t[..i]), clean(&t[i + n..]));
        if a.is_empty() || b.is_empty() {
            return String::new();
        }
        format!("{a}{SLOT_SEP}{b}")
    }

    /// The question before a delete, with the slots filled in.
    pub fn question(&self, filled: &str) -> String {
        let mut q = self.ask.clone();
        for (name, value) in self.slots.iter().zip(filled.split(SLOT_SEP)) {
            q = q.replace(&format!("{{{name}}}"), value);
        }
        q
    }

    /// The request with courtesy and this action's verbs off the front, its
    /// places and courtesy off the end, as often as they come.
    fn strip(&self, request: &str) -> String {
        let mut t = String::from(request.trim());
        let starts = |t: &str, p: &str| {
            t.len() > p.len()
                && t[..p.len()].eq_ignore_ascii_case(p)
                && t[p.len()..].starts_with([' ', ':', ','])
        };
        // Longest first, so "write down" goes before "write".
        let mut front: Vec<&str> = COURTESY_FRONT
            .iter()
            .copied()
            .chain(self.verbs.iter().map(String::as_str))
            .collect();
        front.sort_by_key(|v| core::cmp::Reverse(v.len()));
        let mut back: Vec<String> = self
            .places
            .iter()
            .map(|p| format!(" {p}"))
            .chain(COURTESY_END.iter().map(|s| String::from(*s)))
            .collect();
        back.sort_by_key(|v| core::cmp::Reverse(v.len()));
        loop {
            let before = t.len();
            if let Some(p) = front.iter().find(|p| starts(&t, p)) {
                t = String::from(t[p.len()..].trim_start_matches([' ', ':', ',']));
            }
            let lower = t.to_ascii_lowercase();
            let lower = lower.trim_end_matches(['.', '!', '?', ',']);
            if let Some(s) = back.iter().find(|s| lower.ends_with(s.as_str())) {
                t.truncate(lower.len() - s.len());
                t = String::from(t.trim_end());
            }
            if t.len() == before {
                break;
            }
        }
        t
    }
}

/// A slot's text without surrounding quotes and marks, and without "the
/// word(s)" in front.
fn clean(s: &str) -> String {
    let s = s.trim().trim_matches(['"', '\'', '.', ',', ' ']);
    let l = s.to_ascii_lowercase();
    let s = ["the words ", "the word ", "the line "]
        .iter()
        .find_map(|p| l.starts_with(p).then(|| &s[p.len()..]))
        .unwrap_or(s);
    String::from(s.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    #[test]
    fn phrased_in_its_words() {
        let r = shipped();
        assert_eq!(
            r.phrased("Add milk to Notes").as_deref(),
            Some("write milk")
        );
        assert_eq!(
            r.phrased("add milk to my notes.").as_deref(),
            Some("write milk")
        );
        assert_eq!(
            r.phrased("Add 'delete the old notes' to my notes")
                .as_deref(),
            Some("write delete the old notes")
        );
        assert_eq!(
            r.phrased("delete flowers, egg and milk for my notes")
                .as_deref(),
            Some("remove flowers, egg and milk")
        );
        assert_eq!(
            r.phrased("replace milk with oat milk in my notes")
                .as_deref(),
            Some("change milk\u{1f}oat milk")
        );
        // No place, a one-word place, or no verb: the model's.
        assert_eq!(r.phrased("add milk"), None);
        assert_eq!(r.phrased("put it down"), None);
        assert_eq!(r.phrased("take me out"), None);
        assert_eq!(
            r.phrased("put notes on the left and files on the right"),
            None
        );
        assert_eq!(r.phrased("milk to my notes"), None);
    }

    /// The files shipped in the boot image.
    fn shipped() -> Registry {
        let mut r = Registry::default();
        let mut skipped = Vec::new();
        for f in ["notes", "network"] {
            let path = std::format!(
                "{}/../seed/etc/actions/{f}.actions",
                env!("CARGO_MANIFEST_DIR")
            );
            r.add_file(&std::fs::read_to_string(path).unwrap(), &mut skipped);
        }
        assert!(skipped.is_empty(), "{skipped:?}");
        r
    }

    #[test]
    fn the_shipped_files_load() {
        let r = shipped();
        let words: Vec<&str> = r.actions.iter().map(|a| a.word.as_str()).collect();
        assert_eq!(words, ["write", "remove", "change", "browse"]);
        assert_eq!(r.find("network", "fetch").unwrap().word, "browse");
        assert_eq!(
            r.find_word("change").unwrap().prompt_line(),
            "change <old> to <new>: change words in Notes"
        );
    }

    #[test]
    fn slots_from_the_request() {
        let r = shipped();
        let w = r.find_word("write").unwrap();
        assert_eq!(w.fill("write down buy milk in my notes"), "buy milk");
        assert_eq!(
            w.fill("hey bella take a note: call the vet tomorrow"),
            "call the vet tomorrow"
        );
        assert_eq!(w.fill("can you add eggs to my notes please"), "eggs");
        assert_eq!(w.fill("write buy milk"), "buy milk");
        assert_eq!(
            w.fill("Add flowers, egg and milk to my notes."),
            "flowers, egg and milk"
        );
        let d = r.find_word("remove").unwrap();
        assert_eq!(
            d.fill("delete flowers, egg and milk from my notes"),
            "flowers, egg and milk"
        );
        assert_eq!(d.fill("take milk off my list"), "milk");
        assert_eq!(
            d.fill("can you remove call the vet from my notes please"),
            "call the vet"
        );
        assert_eq!(d.fill("cross eggs off"), "eggs");
        assert_eq!(d.question("milk"), "Remove \"milk\" from your notes?");
        let c = r.find_word("change").unwrap();
        assert_eq!(c.fill("change egg to bread in my notes"), "egg\u{1f}bread");
        assert_eq!(
            c.fill("could you replace the word milk with oat milk"),
            "milk\u{1f}oat milk"
        );
        assert_eq!(c.fill("swap eggs for bread"), "eggs\u{1f}bread");
        assert_eq!(c.fill("change it"), "");
        let b = r.find_word("browse").unwrap();
        assert_eq!(b.fill("go to example.com please"), "example.com");
        assert_eq!(b.fill("open my notes"), "");
    }

    #[test]
    fn invocations_are_checked() {
        let r = shipped();
        assert_eq!(r.check("notes:write:buy milk").unwrap().1, "buy milk");
        assert_eq!(
            r.check("notes:change:egg\u{1f}bread").unwrap().0.name,
            "change"
        );
        assert!(r.check("notes:change:egg").is_none(), "one slot of two");
        assert!(r.check("notes:write:").is_none());
        // The id and a target are not slots.
        assert!(r.check("notes:remove:milk\u{1f}#7").is_some());
        assert!(
            r.check("notes:remove:milk\u{1f}#7\u{1f}@flowers, bread and milk")
                .is_some()
        );
        assert!(r.check("notes:remove:\u{1f}#7").is_none());
        assert_eq!(
            split_arg("egg\u{1f}bread\u{1f}#3\u{1f}@egg two"),
            (alloc::vec!["egg", "bread"], Some("3"), Some("egg two"))
        );
        // A titled note is not a slot either (docs/titled-design.md).
        assert!(r.check("notes:write:socks\u{1f}^packing\u{1f}#7").is_some());
        assert_eq!(
            split_arg("socks\u{1f}^packing\u{1f}#7"),
            (alloc::vec!["socks"], Some("7"), None)
        );
        assert_eq!(note_of("socks\u{1f}^packing\u{1f}#7"), Some("packing"));
        assert_eq!(note_of("socks\u{1f}#7"), None);
        let o = Outcome {
            status: Status::Choose,
            id: Some(String::from("3")),
            said: String::from("Which one?"),
            choices: alloc::vec![String::from("a"), String::from("b")],
        };
        assert_eq!(Outcome::parse(&o.text()), o);
        assert_eq!(Outcome::parse("Wrote \"x\" in notes.").status, Status::Ok);
        assert_eq!(Outcome::parse("Error: no").status, Status::Err);
        assert!(r.check("notes:delete:x").is_none());
        assert!(r.check("files:write:x").is_none());
        assert_eq!(
            r.check("network:fetch:example.com").unwrap().0.effect,
            Effect::Read
        );
    }

    #[test]
    fn bad_entries_are_skipped() {
        let mut r = Registry::default();
        let mut skipped = Vec::new();
        r.add_file(
            "app notes\naction a\n  about x\n  slots text\n  effect maybe\naction b\n  slots text\naction c\n  about y\n  slots one two\naction d\n  about z\n  effect delete\n  slots text\naction e\n  about fine\n  slots text\n",
            &mut skipped,
        );
        assert_eq!(r.actions.len(), 1);
        assert_eq!(r.actions[0].name, "e");
        assert_eq!(skipped.len(), 4, "{skipped:?}");
        let mut again = Vec::new();
        r.add_file(
            "app other\naction e2\n  word e\n  about dup\n  slots text\n",
            &mut again,
        );
        assert_eq!(again, ["other.e2: the word `e` is taken"]);
    }
}
