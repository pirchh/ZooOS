//! English text to Kokoro's phonemes (docs/speech-design.md §6), after
//! misaki's `en.py` (Apache-2.0) without its part-of-speech tagger or
//! neural fallback: misaki's word lists, its special cases for short words
//! ("the", "a", "to", "I") decided by the next word's first sound, its
//! stress rules and its -s, -ed and -ing endings, and numbers in words.
//! A word in no list is split at a change of case ("ZooOS"), or into two
//! known words, or else spelt out.

use alloc::string::String;
use alloc::vec::Vec;

/// misaki's US word lists as sorted `word\tphonemes` lines.
pub struct Lexicon<'a> {
    text: &'a str,
    /// Each line's start and the tab's position.
    lines: Vec<(u32, u32)>,
}

const VOWELS: &str = "AIOQWYaiuæɑɒɔəɛɜɪʊʌᵻ";
const CONSONANTS: &str = "bdfhjklmnpstvwzðŋɡɹɾʃʒʤʧθ";
const PUNCTS: &str = ";:,.!?—…\"“”";
const NON_QUOTE_PUNCTS: &str = ";:,.!?—…";
const PRIMARY: char = 'ˈ';
const SECONDARY: char = 'ˌ';
const US_TAUS: &str = "AIOWYiuæɑəɛɪɹʊʌ";

fn is_vowel(c: char) -> bool {
    VOWELS.contains(c)
}

/// misaki's `apply_stress`. `stress` in tenths: None, -20 (none), -10
/// (secondary), -5, 0, 5, 10, 20.
fn apply_stress(ps: &str, stress: Option<i32>) -> String {
    let Some(s) = stress else {
        return ps.into();
    };
    let has_p = ps.contains(PRIMARY);
    let has_s = ps.contains(SECONDARY);
    let restress = |mark: char| -> String {
        // The mark goes just before the first vowel.
        let mut out = String::new();
        let mut placed = false;
        for c in ps.chars() {
            if !placed && is_vowel(c) {
                out.push(mark);
                placed = true;
            }
            out.push(c);
        }
        out
    };
    let any_vowel = ps.chars().any(is_vowel);
    if s < -10 {
        ps.replace([PRIMARY, SECONDARY], "")
    } else if s == -10 || ((s == 0 || s == -5) && has_p) {
        ps.replace(SECONDARY, "").replace(PRIMARY, "ˌ")
    } else if (s == 0 || s == 5 || s == 10) && !has_p && !has_s {
        if any_vowel {
            restress(SECONDARY)
        } else {
            ps.into()
        }
    } else if s >= 10 && !has_p && has_s {
        ps.replace(SECONDARY, "ˈ")
    } else if s > 10 && !has_p && !has_s {
        if any_vowel {
            restress(PRIMARY)
        } else {
            ps.into()
        }
    } else {
        ps.into()
    }
}

/// What the next word starts with: a vowel, a consonant, or nothing
/// (punctuation or the end), for "the", "to" and "am".
#[derive(Clone, Copy, PartialEq)]
enum Next {
    Vowel,
    Consonant,
    Nothing,
}

fn next_of(ps: &str, before: Next) -> Next {
    for c in ps.chars() {
        if NON_QUOTE_PUNCTS.contains(c) {
            return Next::Nothing;
        }
        if is_vowel(c) {
            return Next::Vowel;
        }
        if CONSONANTS.contains(c) {
            return Next::Consonant;
        }
    }
    before
}

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// A number in words, as num2words writes it without "and".
fn cardinal(n: u64, out: &mut Vec<&'static str>) {
    if n < 20 {
        out.push(ONES[n as usize]);
    } else if n < 100 {
        out.push(TENS[(n / 10) as usize]);
        if !n.is_multiple_of(10) {
            out.push(ONES[(n % 10) as usize]);
        }
    } else if n < 1000 {
        out.push(ONES[(n / 100) as usize]);
        out.push("hundred");
        if !n.is_multiple_of(100) {
            cardinal(n % 100, out);
        }
    } else {
        for (scale, name) in [
            (1_000_000_000, "billion"),
            (1_000_000, "million"),
            (1000, "thousand"),
        ] {
            if n >= scale {
                cardinal(n / scale, out);
                out.push(name);
                if !n.is_multiple_of(scale) {
                    cardinal(n % scale, out);
                }
                return;
            }
        }
    }
}

/// A year as said: 1999 nineteen ninety-nine, 2026 twenty twenty-six,
/// 2000 two thousand, 1905 nineteen oh five.
fn year(n: u64, out: &mut Vec<&'static str>) {
    let (hi, lo) = (n / 100, n % 100);
    if n.is_multiple_of(1000) || (2001..=2009).contains(&n) {
        cardinal(n, out);
    } else if lo == 0 {
        cardinal(hi, out);
        out.push("hundred");
    } else {
        cardinal(hi, out);
        if lo < 10 {
            out.push("oh");
        }
        cardinal(lo, out);
    }
}

fn ordinal_word(w: &str) -> String {
    let irregular = [
        ("one", "first"),
        ("two", "second"),
        ("three", "third"),
        ("five", "fifth"),
        ("eight", "eighth"),
        ("nine", "ninth"),
        ("twelve", "twelfth"),
    ];
    if let Some((_, o)) = irregular.iter().find(|(c, _)| *c == w) {
        return (*o).into();
    }
    if let Some(stem) = w.strip_suffix('y') {
        return alloc::format!("{stem}ieth");
    }
    alloc::format!("{w}th")
}

impl<'a> Lexicon<'a> {
    /// From sorted `word\tphonemes` lines (`cargo xtask speak-gguf`).
    pub fn new(text: &'a str) -> Lexicon<'a> {
        let mut lines = Vec::new();
        let mut start = 0usize;
        for line in text.split_inclusive('\n') {
            if let Some(tab) = line.find('\t') {
                lines.push((start as u32, (start + tab) as u32));
            }
            start += line.len();
        }
        Lexicon { text, lines }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The listed phonemes for exactly `word`.
    pub fn get(&self, word: &str) -> Option<&'a str> {
        let i = self
            .lines
            .binary_search_by(|&(s, t)| self.text[s as usize..t as usize].cmp(word))
            .ok()?;
        let (_, t) = self.lines[i];
        let rest = &self.text[t as usize + 1..];
        Some(rest.split('\n').next().unwrap_or("").trim_end_matches('\r'))
    }

    /// `word` as listed, or its lower-case or capitalised form (misaki's
    /// `grow_dictionary`).
    fn find(&self, word: &str) -> Option<&'a str> {
        if let Some(p) = self.get(word) {
            return Some(p);
        }
        let lower = word.to_lowercase();
        if let Some(p) = self.get(&lower) {
            return Some(p);
        }
        let mut cap = String::new();
        for (i, c) in lower.chars().enumerate() {
            if i == 0 {
                cap.extend(c.to_uppercase());
            } else {
                cap.push(c);
            }
        }
        self.get(&cap)
    }

    /// Letters spelt out: "OS" as O, S (misaki's `get_NNP`).
    fn spell(&self, word: &str) -> Option<String> {
        let mut ps = String::new();
        for c in word.chars().filter(|c| c.is_ascii_alphabetic()) {
            let mut u = String::new();
            u.push(c.to_ascii_uppercase());
            ps.push_str(self.get(&u)?);
        }
        let ps = apply_stress(&ps, Some(0));
        // The last secondary stress becomes the primary.
        match ps.rfind(SECONDARY) {
            Some(i) => {
                let mut o = String::from(&ps[..i]);
                o.push(PRIMARY);
                o.push_str(&ps[i + SECONDARY.len_utf8()..]);
                Some(o)
            }
            None => Some(ps),
        }
    }

    fn with_s(stem: &str) -> String {
        let last = stem.chars().last().unwrap_or(' ');
        if "ptkfθ".contains(last) {
            alloc::format!("{stem}s")
        } else if "szʃʒʧʤ".contains(last) {
            alloc::format!("{stem}ᵻz")
        } else {
            alloc::format!("{stem}z")
        }
    }

    fn with_ed(stem: &str) -> String {
        let cs: Vec<char> = stem.chars().collect();
        let last = *cs.last().unwrap_or(&' ');
        if "pkfθʃsʧ".contains(last) {
            alloc::format!("{stem}t")
        } else if last == 'd' {
            alloc::format!("{stem}ᵻd")
        } else if last != 't' {
            alloc::format!("{stem}d")
        } else if cs.len() >= 2 && US_TAUS.contains(cs[cs.len() - 2]) {
            let head: String = cs[..cs.len() - 1].iter().collect();
            alloc::format!("{head}ɾᵻd")
        } else {
            alloc::format!("{stem}ᵻd")
        }
    }

    fn with_ing(stem: &str) -> String {
        let cs: Vec<char> = stem.chars().collect();
        if cs.len() > 1 && cs[cs.len() - 1] == 't' && US_TAUS.contains(cs[cs.len() - 2]) {
            let head: String = cs[..cs.len() - 1].iter().collect();
            alloc::format!("{head}ɾɪŋ")
        } else {
            alloc::format!("{stem}ɪŋ")
        }
    }

    /// The word's phonemes from the lists and their endings.
    fn listed(&self, w: &str) -> Option<String> {
        if let Some(p) = self.find(w) {
            return Some(p.into());
        }
        let n = w.len();
        if n >= 3 && w.ends_with('s') {
            if !w.ends_with("ss")
                && let Some(p) = self.find(&w[..n - 1])
            {
                return Some(Self::with_s(p));
            }
            if (w.ends_with("'s") || (n > 4 && w.ends_with("es") && !w.ends_with("ies")))
                && let Some(p) = self.find(&w[..n - 2])
            {
                return Some(Self::with_s(p));
            }
            if n > 4
                && w.ends_with("ies")
                && let Some(p) = self.find(&alloc::format!("{}y", &w[..n - 3]))
            {
                return Some(Self::with_s(p));
            }
        }
        if n >= 4 && w.ends_with('d') {
            if !w.ends_with("dd")
                && let Some(p) = self.find(&w[..n - 1])
            {
                return Some(Self::with_ed(p));
            }
            if n > 4
                && w.ends_with("ed")
                && !w.ends_with("eed")
                && let Some(p) = self.find(&w[..n - 2])
            {
                return Some(Self::with_ed(p));
            }
        }
        if n >= 5 && w.ends_with("ing") {
            let stem = &w[..n - 3];
            let p = if n > 5 { self.find(stem) } else { None }
                .or_else(|| self.find(&alloc::format!("{stem}e")))
                .or_else(|| {
                    let b = stem.as_bytes();
                    if n > 5 && b.len() >= 2 && b[b.len() - 1] == b[b.len() - 2] {
                        self.find(&stem[..stem.len() - 1])
                    } else {
                        None
                    }
                })?;
            return Some(Self::with_ing(&apply_stress(p, Some(5))));
        }
        None
    }

    /// A word misaki's lists don't have.
    fn unlisted(&self, w: &str) -> Option<String> {
        let letters = w.chars().all(|c| c.is_ascii_alphabetic() || c == '\'');
        if !letters {
            return None;
        }
        // A change of case: "ZooOS" is "Zoo" and "OS".
        let cs: Vec<char> = w.chars().collect();
        for i in 1..cs.len() {
            let split = (cs[i - 1].is_ascii_lowercase() && cs[i].is_ascii_uppercase())
                || (i + 1 < cs.len()
                    && cs[i - 1].is_ascii_uppercase()
                    && cs[i].is_ascii_uppercase()
                    && cs[i + 1].is_ascii_lowercase());
            if split {
                let (a, b): (String, String) = (cs[..i].iter().collect(), cs[i..].iter().collect());
                if let (Some(pa), Some(pb)) = (self.word(&a, None), self.word(&b, None)) {
                    return Some(alloc::format!("{pa}{pb}"));
                }
            }
        }
        let all_caps = cs.len() > 1 && cs.iter().all(|c| c.is_ascii_uppercase());
        if !all_caps && cs.len() > 4 {
            // Two known words ("dropdown"), the longest first part first.
            for i in (2..cs.len() - 1).rev() {
                let (a, b): (String, String) = (cs[..i].iter().collect(), cs[i..].iter().collect());
                if a.len() >= 2
                    && b.len() >= 2
                    && let (Some(pa), Some(pb)) = (self.listed(&a), self.listed(&b))
                {
                    return Some(alloc::format!("{pa}{pb}"));
                }
            }
        }
        self.spell(w)
    }

    /// One word's phonemes, given what follows it.
    fn word(&self, w: &str, next: Option<Next>) -> Option<String> {
        let next = next.unwrap_or(Next::Nothing);
        match w {
            // As a determiner, as it nearly always is.
            "a" | "A" => return Some("ɐ".into()),
            "an" | "An" => return Some("ɐn".into()),
            "I" => return Some("ˌI".into()),
            "to" | "To" => {
                return Some(
                    match next {
                        Next::Nothing => self.get("to").unwrap_or("tu"),
                        Next::Consonant => "tə",
                        Next::Vowel => "tʊ",
                    }
                    .into(),
                );
            }
            "the" | "The" => {
                return Some(if next == Next::Vowel { "ði" } else { "ðə" }.into());
            }
            "in" | "In" => {
                return Some(
                    if next == Next::Nothing {
                        "ˈɪn"
                    } else {
                        "ɪn"
                    }
                    .into(),
                );
            }
            "am" => {
                return Some(if next == Next::Nothing {
                    self.get("am").unwrap_or("æm").into()
                } else {
                    "ɐm".into()
                });
            }
            _ => {}
        }
        // At the end of a sentence some words are said otherwise ("won't"
        // stressed): misaki's `None` form, listed as `word|end`.
        if next == Next::Nothing
            && let Some(p) = self.get(&alloc::format!("{w}|end"))
        {
            return Some(p.into());
        }
        let lower = w.chars().all(|c| !c.is_uppercase());
        let all_caps = w.chars().count() > 1 && w.chars().all(|c| !c.is_lowercase());
        let stress = if lower {
            None
        } else if all_caps {
            Some(20)
        } else {
            Some(5)
        };
        // Capitalised for the sentence, but listed in lower case.
        let ps = if !lower && self.get(w).is_none() {
            self.listed(&w.to_lowercase()).or_else(|| self.listed(w))
        } else {
            self.listed(w)
        };
        let ps = match ps {
            Some(p) => p,
            None => return self.unlisted(w),
        };
        Some(apply_stress(&ps, stress))
    }

    fn number(&self, t: &str) -> Option<String> {
        let suffix_at = t
            .find(|c: char| c.is_ascii_alphabetic() || c == '\'')
            .unwrap_or(t.len());
        let (num, suffix) = t.split_at(suffix_at);
        let digits: String = num.chars().filter(|c| *c != ',').collect();
        let mut words: Vec<&'static str> = Vec::new();
        let mut owned: Vec<String> = Vec::new();
        if let Some((a, b)) = digits.split_once('.') {
            cardinal(a.parse().ok()?, &mut words);
            words.push("point");
            for d in b.chars() {
                words.push(ONES[d.to_digit(10)? as usize]);
            }
        } else {
            let n: u64 = digits.parse().ok()?;
            if matches!(suffix, "st" | "nd" | "rd" | "th") {
                cardinal(n, &mut words);
                let last = words.pop()?;
                owned.push(ordinal_word(last));
            } else if digits.len() == 4 && (1100..3000).contains(&n) {
                year(n, &mut words);
            } else {
                cardinal(n, &mut words);
            }
        }
        let mut ps = Vec::new();
        for w in words
            .iter()
            .copied()
            .chain(owned.iter().map(String::as_str))
        {
            let p = self.listed(w)?;
            ps.push(p);
        }
        let mut s = ps.join(" ");
        if matches!(suffix, "s" | "'s") {
            s = Self::with_s(&s);
        }
        Some(s)
    }

    /// `text` as Kokoro's phonemes.
    pub fn phonemes(&self, text: &str) -> String {
        #[derive(Clone)]
        enum Tok {
            Word(String),
            Num(String),
            Punct(char),
            Space,
        }
        let text = text.replace(['‘', '’'], "'");
        let cs: Vec<char> = text.chars().collect();
        let mut toks = Vec::new();
        let mut i = 0;
        while i < cs.len() {
            let c = cs[i];
            if c.is_ascii_digit() {
                let mut s = String::new();
                while i < cs.len()
                    && (cs[i].is_ascii_digit()
                        || ((cs[i] == ',' || cs[i] == '.')
                            && i + 1 < cs.len()
                            && cs[i + 1].is_ascii_digit()))
                {
                    s.push(cs[i]);
                    i += 1;
                }
                while i < cs.len() && (cs[i].is_ascii_alphabetic() || cs[i] == '\'') {
                    s.push(cs[i]);
                    i += 1;
                }
                toks.push(Tok::Num(s));
            } else if c.is_alphabetic() {
                let mut s = String::new();
                while i < cs.len()
                    && (cs[i].is_alphabetic()
                        || (cs[i] == '\'' && i + 1 < cs.len() && cs[i + 1].is_alphabetic()))
                {
                    s.push(cs[i]);
                    i += 1;
                }
                toks.push(Tok::Word(s));
            } else if c.is_whitespace() || c == '-' || c == '_' || c == '/' {
                if !matches!(toks.last(), Some(Tok::Space)) {
                    toks.push(Tok::Space);
                }
                i += 1;
            } else {
                let word = match c {
                    '%' => Some("percent"),
                    '&' => Some("and"),
                    '+' => Some("plus"),
                    '@' => Some("at"),
                    _ => None,
                };
                if let Some(w) = word {
                    toks.push(Tok::Word(w.into()));
                } else if PUNCTS.contains(c) {
                    toks.push(Tok::Punct(c));
                }
                i += 1;
            }
        }
        // Right to left, so each word knows how the next one starts.
        let mut ps: Vec<String> = alloc::vec![String::new(); toks.len()];
        let mut next = Next::Nothing;
        for (k, t) in toks.iter().enumerate().rev() {
            match t {
                Tok::Word(w) => {
                    let p = self.word(w, Some(next)).unwrap_or_default();
                    next = next_of(&p, next);
                    ps[k] = p;
                }
                Tok::Num(n) => {
                    let p = self.number(n).unwrap_or_default();
                    next = next_of(&p, next);
                    ps[k] = p;
                }
                Tok::Punct(c) => {
                    ps[k].push(*c);
                    next = next_of(&ps[k], next);
                }
                Tok::Space => {}
            }
        }
        let mut out = String::new();
        for (k, t) in toks.iter().enumerate() {
            match t {
                Tok::Space => {
                    if !out.is_empty() && !out.ends_with(' ') {
                        out.push(' ');
                    }
                }
                Tok::Punct(_) => {
                    // Punctuation sits on the word before it.
                    if out.ends_with(' ') && !matches!(ps[k].as_str(), "“" | "\"") {
                        out.pop();
                    }
                    out.push_str(&ps[k]);
                }
                _ => {
                    if !ps[k].is_empty() {
                        if !out.is_empty() && !out.ends_with(' ') && !out.ends_with(['“', '"']) {
                            out.push(' ');
                        }
                        out.push_str(&ps[k]);
                    }
                }
            }
        }
        String::from(out.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    const LIST: &str = "A\tˈA\nB\tbˈi\nBella\tbˈɛlə\nO\tˈO\nS\tˈɛs\nam\tæm\nbella\tbˈɛlə\ndentist\tdˈɛntɪst\nfive\tfˈIv\nis\tɪz\non\tɑn\nfriday\tfɹˈIdˌA\nhello\thəlˈO\nmaximize\tmˈæksəmˌIz\nminimize\tmˈɪnəmˌIz\nnote\tnˈOt\nnotes\tnˈOts\nopen\tˈOpən\nsix\tsˈɪks\nthe\tði\ntwenty\ttwˈɛnti\nzoo\tzˈu\n";

    fn lex() -> Lexicon<'static> {
        let mut lines: Vec<&str> = LIST.lines().collect();
        lines.sort();
        let s: String = lines.iter().map(|l| alloc::format!("{l}\n")).collect();
        Lexicon::new(std::boxed::Box::leak(s.into_boxed_str()))
    }

    #[test]
    fn sentences() {
        let l = lex();
        assert_eq!(l.phonemes("Hello, I am Bella."), "həlˈO, ˌI ɐm bˈɛlə.");
        assert_eq!(
            l.phonemes("The dentist is on Friday"),
            "ðə dˈɛntɪst ɪz ɑn fɹˈIdˌA"
        );
        assert_eq!(l.phonemes("open the notes"), "ˈOpən ðə nˈOts");
    }

    #[test]
    fn endings_and_case() {
        let l = lex();
        assert_eq!(l.phonemes("minimized"), "mˈɪnəmˌIzd");
        assert_eq!(l.phonemes("opened"), "ˈOpənd");
        assert_eq!(l.phonemes("ZooOS"), "zˈuˌOˈɛs");
        assert_eq!(l.phonemes("26"), "twˈɛnti sˈɪks");
    }

    #[test]
    fn stress_rules() {
        assert_eq!(apply_stress("ˈOpən", Some(-10)), "ˌOpən");
        assert_eq!(apply_stress("bɛlə", Some(20)), "bˈɛlə");
        assert_eq!(apply_stress("bˈɛlə", Some(-20)), "bɛlə");
    }
}
