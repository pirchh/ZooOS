//! Whether something heard is said to Bella, from what it means
//! (docs/addressee-design.md). Used for speech that follows her answer
//! without her name and that she can't act on without the model ("and
//! what about Photos?", "what time is it?"): the model reads her last
//! answer and the sentence, and the scores of "yes" and "no" decide. No
//! text is generated, so it costs one pass over the sentence.
//!
//! The system prompt is fed once (`Judge::new`) and its key/value cache is
//! reused for every sentence by rewinding the position.

use crate::assist::AssistError;
use crate::chat;
use crate::model::{Model, State};
use crate::tokenizer::Tokenizer;
use alloc::string::String;
use alloc::vec::Vec;

/// What the judge is told.
pub const SYSTEM: &str = "You are listening in a room with a computer assistant called Bella.\n\
Bella has just answered the user. Then someone in the room says a sentence.\n\
Decide whether that sentence is said to Bella: a request, a question or a reply meant for the assistant.\n\
Talk to another person, thinking aloud, or talk about Bella is not said to Bella.\n\
Answer with one word: yes or no.";

/// Said to her when "yes" leads "no" by at least this much (set from
/// `cargo xtask bella-addressee`; docs/addressee-design.md).
pub const THRESHOLD: f32 = 0.0;

/// Most bytes of her answer and of the sentence.
pub const MAX_TEXT: usize = 200;

/// How sure the judge is: the score of "yes" minus that of "no".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    pub margin: f32,
    pub tokens: usize,
}

impl Verdict {
    /// Said to her when "yes" leads by at least `threshold`.
    pub fn for_her(&self, threshold: f32) -> bool {
        self.margin >= threshold
    }
}

pub struct Judge<'m, 't> {
    model: &'m Model<'m>,
    tok: &'t Tokenizer<'t>,
    st: State,
    prefix_len: usize,
    yes: Vec<u32>,
    no: Vec<u32>,
}

/// The first token of each spelling of `word`, without repeats.
fn first_tokens(tok: &Tokenizer, spellings: &[&str]) -> Vec<u32> {
    let mut ids = Vec::new();
    for s in spellings {
        if let Some(&id) = tok.encode(s).first()
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}

impl<'m, 't> Judge<'m, 't> {
    /// Feeds the system prompt.
    pub fn new(model: &'m Model<'m>, tok: &'t Tokenizer<'t>) -> Result<Self, AssistError> {
        let full = chat::prompt(SYSTEM, "");
        let cut = full
            .rfind("<|im_start|>user\n")
            .map(|i| i + "<|im_start|>user\n".len())
            .unwrap_or(full.len());
        let prefix_ids = tok.encode(&full[..cut]);
        // Prefix, two texts of at most MAX_TEXT bytes, the framing.
        let ctx = prefix_ids.len() + 2 * MAX_TEXT + 64;
        let mut st = model.new_state(ctx);
        model.read_prompt(&mut st, &prefix_ids, false, |_| true)?;
        Ok(Judge {
            model,
            tok,
            st,
            prefix_len: prefix_ids.len(),
            yes: first_tokens(tok, &["yes", "Yes", "YES"]),
            no: first_tokens(tok, &["no", "No", "NO"]),
        })
    }

    /// How sure it is that `heard`, said after Bella answered `answer`
    /// (empty when she said nothing aloud), is said to her.
    pub fn judge(&mut self, answer: &str, heard: &str) -> Result<Verdict, AssistError> {
        let (answer, heard) = (answer.trim(), heard.trim());
        if heard.is_empty() {
            return Err(AssistError::Empty);
        }
        if answer.len() > MAX_TEXT || heard.len() > MAX_TEXT {
            return Err(AssistError::TooLong);
        }
        let mut text = String::new();
        if !answer.is_empty() {
            text.push_str("Bella said: \"");
            text.push_str(answer);
            text.push_str("\"\n");
        }
        text.push_str("Then someone said: \"");
        text.push_str(heard);
        text.push_str("\"\nIs that said to Bella?");
        text.push_str(chat::reply_open(self.model));
        let ids = self.tok.encode(&text);
        if self.prefix_len + ids.len() > self.st_capacity() {
            return Err(AssistError::TooLong);
        }
        self.st.pos = self.prefix_len;
        self.model.read_prompt(&mut self.st, &ids, true, |_| true)?;
        let best = |set: &[u32]| {
            set.iter()
                .map(|&id| self.st.logits[id as usize])
                .fold(f32::NEG_INFINITY, f32::max)
        };
        Ok(Verdict {
            margin: best(&self.yes) - best(&self.no),
            tokens: ids.len(),
        })
    }

    fn st_capacity(&self) -> usize {
        self.prefix_len + 2 * MAX_TEXT + 64
    }

    /// Tokens in the system prompt.
    pub fn prefix_tokens(&self) -> usize {
        self.prefix_len
    }
}
