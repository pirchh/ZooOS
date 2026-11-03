//! Qwen2 tokenizer: GPT-2-style byte-level BPE, with the vocabulary and
//! merges read from the GGUF file (docs/m5b-design.md §3).
//!
//! Pre-tokenization reimplements Qwen2's split pattern
//! `(?i:'s|'t|'re|'ve|'m|'ll|'d)|[^\r\n\p{L}\p{N}]?\p{L}+|\p{N}| ?[^\s\p{L}\p{N}]+[\r\n]*|\s*[\r\n]+|\s+(?!\S)|\s+`
//! by hand, using `char::is_alphabetic`/`is_numeric`/`is_whitespace` for the
//! Unicode classes (close to, not exactly, `\p{L}`/`\p{N}`).

use crate::gguf::{Gguf, GgufError};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// GGUF token type of control tokens such as `<|im_start|>`.
const TOKEN_CONTROL: i32 = 3;
/// GGUF token type of added tokens such as Qwen3's `<think>`: matched
/// literally in input like control tokens, but printable in output.
const TOKEN_USER_DEFINED: i32 = 4;

pub struct Tokenizer<'a> {
    tokens: Vec<&'a str>,
    vocab: BTreeMap<&'a str, u32>,
    /// (left id, right id) -> (merge rank, merged id).
    merges: BTreeMap<(u32, u32), (u32, u32)>,
    /// Control tokens matched literally in input text, longest first.
    special: Vec<(&'a str, u32)>,
    is_control: Vec<bool>,
    byte_to_char: [char; 256],
    char_to_byte: BTreeMap<char, u8>,
    pub eos: u32,
}

/// GPT-2's reversible byte <-> printable character mapping.
fn byte_chars() -> [char; 256] {
    let mut map = ['\0'; 256];
    let mut n = 0u32;
    for b in 0..256u32 {
        let printable =
            (33..=126).contains(&b) || (161..=172).contains(&b) || (174..=255).contains(&b);
        map[b as usize] = if printable {
            char::from_u32(b).unwrap_or('?')
        } else {
            n += 1;
            char::from_u32(255 + n).unwrap_or('?')
        };
    }
    map
}

impl<'a> Tokenizer<'a> {
    pub fn from_gguf(g: &Gguf<'a>) -> Result<Tokenizer<'a>, GgufError> {
        let tokens = g.strings("tokenizer.ggml.tokens")?;
        let types = g.i32s("tokenizer.ggml.token_type")?;
        let merge_list = g.strings("tokenizer.ggml.merges")?;
        let eos = g.u32("tokenizer.ggml.eos_token_id")?;
        let mut vocab = BTreeMap::new();
        for (i, t) in tokens.iter().enumerate() {
            vocab.insert(*t, i as u32);
        }
        let mut merges = BTreeMap::new();
        let mut joined = String::new();
        for (rank, m) in merge_list.iter().enumerate() {
            let Some((a, b)) = m.split_once(' ') else {
                continue;
            };
            joined.clear();
            joined.push_str(a);
            joined.push_str(b);
            if let (Some(&ia), Some(&ib), Some(&im)) =
                (vocab.get(a), vocab.get(b), vocab.get(joined.as_str()))
            {
                merges.entry((ia, ib)).or_insert((rank as u32, im));
            }
        }
        let is_control: Vec<bool> = (0..tokens.len())
            .map(|i| types.get(i) == Some(&TOKEN_CONTROL))
            .collect();
        let mut special: Vec<(&str, u32)> = tokens
            .iter()
            .enumerate()
            .filter(|(i, _)| is_control[*i] || types.get(*i) == Some(&TOKEN_USER_DEFINED))
            .map(|(i, t)| (*t, i as u32))
            .collect();
        special.sort_by_key(|(t, _)| core::cmp::Reverse(t.len()));
        let byte_to_char = byte_chars();
        let char_to_byte = byte_to_char
            .iter()
            .enumerate()
            .map(|(b, &c)| (c, b as u8))
            .collect();
        Ok(Tokenizer {
            tokens,
            vocab,
            merges,
            special,
            is_control,
            byte_to_char,
            char_to_byte,
            eos,
        })
    }

    pub fn vocab_size(&self) -> usize {
        self.tokens.len()
    }

    /// Encodes `text`, recognising control tokens like `<|im_start|>`.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut out = Vec::new();
        let mut rest = text;
        while !rest.is_empty() {
            // Earliest control token in the remaining text.
            let next = self
                .special
                .iter()
                .filter_map(|&(s, id)| rest.find(s).map(|at| (at, s.len(), id)))
                .min_by_key(|&(at, len, _)| (at, core::cmp::Reverse(len)));
            let (plain, after) = match next {
                Some((at, len, id)) => {
                    let plain = &rest[..at];
                    self.encode_plain(plain, &mut out);
                    out.push(id);
                    (None, &rest[at + len..])
                }
                None => (Some(rest), ""),
            };
            if let Some(p) = plain {
                self.encode_plain(p, &mut out);
            }
            rest = after;
        }
        out
    }

    fn encode_plain(&self, text: &str, out: &mut Vec<u32>) {
        for piece in pre_tokenize(text) {
            self.bpe(piece, out);
        }
    }

    /// Byte-level BPE on one pre-tokenized piece.
    fn bpe(&self, piece: &str, out: &mut Vec<u32>) {
        let mut syms: Vec<u32> = Vec::new();
        let mut buf = [0u8; 4];
        for &b in piece.as_bytes() {
            let c = self.byte_to_char[b as usize];
            match self.vocab.get(c.encode_utf8(&mut buf) as &str) {
                Some(&id) => syms.push(id),
                None => return, // cannot happen for a byte-level vocabulary
            }
        }
        loop {
            let best = syms
                .windows(2)
                .enumerate()
                .filter_map(|(i, w)| {
                    self.merges
                        .get(&(w[0], w[1]))
                        .map(|&(rank, id)| (rank, i, id))
                })
                .min();
            let Some((_, i, id)) = best else { break };
            syms[i] = id;
            syms.remove(i + 1);
        }
        out.extend_from_slice(&syms);
    }

    /// The raw bytes of `id` (control tokens decode to their literal text).
    pub fn token_bytes(&self, id: u32, out: &mut Vec<u8>) {
        let Some(t) = self.tokens.get(id as usize) else {
            return;
        };
        if self.is_control.get(id as usize) == Some(&true) {
            out.extend_from_slice(t.as_bytes());
            return;
        }
        for c in t.chars() {
            match self.char_to_byte.get(&c) {
                Some(&b) => out.push(b),
                None => out.extend_from_slice(c.encode_utf8(&mut [0u8; 4]).as_bytes()),
            }
        }
    }

    pub fn is_control(&self, id: u32) -> bool {
        self.is_control.get(id as usize) == Some(&true)
    }

    /// Decodes ids to text, skipping control tokens; invalid UTF-8 is
    /// replaced.
    pub fn decode(&self, ids: &[u32]) -> String {
        let mut bytes = Vec::new();
        for &id in ids {
            if !self.is_control(id) {
                self.token_bytes(id, &mut bytes);
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

fn is_letter(c: char) -> bool {
    c.is_alphabetic()
}
fn is_number(c: char) -> bool {
    c.is_numeric()
}
fn is_punct(c: char) -> bool {
    !c.is_whitespace() && !is_letter(c) && !is_number(c)
}

/// Splits text into pieces per Qwen2's pattern (see module docs).
pub fn pre_tokenize(text: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let byte = |i: usize| chars.get(i).map_or(text.len(), |&(b, _)| b);
    let mut pieces = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i].1;
        let end = (|| {
            // 1. Contractions.
            if c == '\'' {
                for suffix in ["s", "t", "re", "ve", "m", "ll", "d"] {
                    let len = suffix.len();
                    let ok = suffix
                        .chars()
                        .enumerate()
                        .all(|(k, s)| at(i + 1 + k).is_some_and(|x| x.to_ascii_lowercase() == s));
                    if ok {
                        return i + 1 + len;
                    }
                }
            }
            // 2. Optional non-letter/non-number (not newline), then letters.
            let mut j = i;
            if !is_letter(c)
                && !is_number(c)
                && c != '\r'
                && c != '\n'
                && at(i + 1).is_some_and(is_letter)
            {
                j = i + 1;
            }
            if at(j).is_some_and(is_letter) {
                while at(j).is_some_and(is_letter) {
                    j += 1;
                }
                return j;
            }
            // 3. One number character.
            if is_number(c) {
                return i + 1;
            }
            // 4. Optional space, punctuation run, trailing newlines.
            let mut j = i;
            if c == ' ' && at(i + 1).is_some_and(is_punct) {
                j = i + 1;
            }
            if at(j).is_some_and(is_punct) {
                while at(j).is_some_and(is_punct) {
                    j += 1;
                }
                while at(j).is_some_and(|x| x == '\r' || x == '\n') {
                    j += 1;
                }
                return j;
            }
            // Whitespace alternatives.
            let mut k = i;
            while at(k).is_some_and(char::is_whitespace) {
                k += 1;
            }
            if k > i {
                // 5. Whitespace up to and including the last newline in it.
                if let Some(last_nl) = (i..k).rev().find(|&p| matches!(at(p), Some('\r' | '\n'))) {
                    return last_nl + 1;
                }
                // 6. Whitespace not followed by a non-space (leave one).
                if k == n {
                    return k;
                }
                if k - i >= 2 {
                    return k - 1;
                }
                // 7. Plain whitespace.
                return k;
            }
            i + 1
        })();
        pieces.push(&text[byte(i)..byte(end)]);
        i = end;
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_tokenizer_splits_like_the_qwen2_pattern() {
        assert_eq!(pre_tokenize("Hello, world!"), ["Hello", ",", " world", "!"]);
        assert_eq!(
            pre_tokenize("I'm 42 years"),
            ["I", "'m", " ", "4", "2", " years"]
        );
        assert_eq!(pre_tokenize("a  b"), ["a", " ", " b"]);
        assert_eq!(pre_tokenize("line\n\nnext"), ["line", "\n\n", "next"]);
        assert_eq!(pre_tokenize("x ... y"), ["x", " ...", " y"]);
        assert_eq!(pre_tokenize("end  "), ["end", "  "]);
        assert_eq!(pre_tokenize("café ok"), ["café", " ok"]);
    }

    #[test]
    fn byte_mapping_is_reversible() {
        let m = byte_chars();
        let mut seen = alloc::collections::BTreeSet::new();
        for c in m {
            assert!(seen.insert(c));
        }
        assert_eq!(m[b' ' as usize], 'Ġ');
        assert_eq!(m[b'\n' as usize], 'Ċ');
        assert_eq!(m[b'A' as usize], 'A');
    }
}
