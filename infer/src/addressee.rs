//! Whether something heard is said to Bella, from what it means
//! (docs/addressee-design.md). Used for speech that follows her answer
//! without her name and that she can't act on without the model ("and
//! what about Photos?", "what time is it?").
//!
//! The model is not asked a question: a 1.7B model told to answer
//! "yes"/"no" (or "computer"/"person") gave the same answer to every
//! sentence (docs/addressee-design.md §2). Instead it reads the sentence
//! twice, once as said to Bella and once as said to someone else in the
//! room, and the difference in how likely it finds the words decides.
//! That uses what a language model does best, predicting text. Her answer
//! is read once and both readings continue from it.
//!
//! Not used by Bella: with Qwen3-1.7B it takes 8 of 16 sentences of talk
//! and needs about 8 s per sentence on the host. `cargo xtask
//! bella-addressee` measures it, for the model bake-off.

use crate::assist::AssistError;
use crate::model::{Model, State};
use crate::tokenizer::Tokenizer;
use alloc::string::String;

/// Said to her when the words are at least this much more likely (in
/// nats, summed over the sentence) as said to her than to someone else
/// (set from `cargo xtask bella-addressee`; docs/addressee-design.md).
pub const THRESHOLD: f32 = 0.0;

/// Most bytes of her answer and of the sentence.
pub const MAX_TEXT: usize = 200;

/// The text both readings share, before her answer.
const OPEN: &str = "At home, a person is using a computer with a voice assistant called Bella. \
Bella opens, closes and arranges apps and windows, finds and opens files, reads and writes notes and lists, and tells the time and date. \
Bella has just said aloud: \"";

/// Read as said to her.
const TO_HER: &str = "\"\nThe person then says to Bella: \"";

/// Read as said to someone else.
const TO_OTHER: &str = "\"\nThe person then turns to someone else in the room and says: \"";

/// How sure the judge is: how much more likely the words are as said to
/// her, in nats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    pub margin: f32,
    pub tokens: usize,
}

impl Verdict {
    /// Said to her when the margin is at least `threshold`.
    pub fn for_her(&self, threshold: f32) -> bool {
        self.margin >= threshold
    }
}

pub struct Judge<'m, 't> {
    model: &'m Model<'m>,
    tok: &'t Tokenizer<'t>,
    st: State,
}

fn capacity(tok_open: usize) -> usize {
    // The opening, her answer, the longer framing, the sentence; text is
    // never fewer bytes than tokens.
    tok_open + MAX_TEXT + 32 + MAX_TEXT + 8
}

impl<'m, 't> Judge<'m, 't> {
    pub fn new(model: &'m Model<'m>, tok: &'t Tokenizer<'t>) -> Result<Self, AssistError> {
        let st = model.new_state(capacity(OPEN.len()));
        Ok(Judge { model, tok, st })
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
        let mut shared = String::from(OPEN);
        shared.push_str(if answer.is_empty() { "…" } else { answer });
        let shared = self.tok.encode(&shared);
        self.st.pos = 0;
        self.model
            .read_prompt(&mut self.st, &shared, false, |_| true)?;
        let words = self.tok.encode(heard);
        let to_her = self.likelihood(shared.len(), TO_HER, &words)?;
        let to_other = self.likelihood(shared.len(), TO_OTHER, &words)?;
        Ok(Verdict {
            margin: to_her - to_other,
            tokens: shared.len() + words.len(),
        })
    }

    /// The log-likelihood of `words` after the shared text (`at` tokens
    /// in) and `framing`.
    fn likelihood(&mut self, at: usize, framing: &str, words: &[u32]) -> Result<f32, AssistError> {
        let framing = self.tok.encode(framing);
        self.st.pos = at;
        self.model
            .read_prompt(&mut self.st, &framing, true, |_| true)?;
        let mut sum = 0.0;
        for (i, &id) in words.iter().enumerate() {
            sum += log_softmax_at(&self.st.logits, id as usize);
            if i + 1 < words.len() {
                self.model.forward(&mut self.st, id)?;
            }
        }
        Ok(sum)
    }
}

/// `ln p(id)` from logits.
fn log_softmax_at(logits: &[f32], id: usize) -> f32 {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = logits.iter().map(|&l| libm::expf(l - max)).sum();
    logits[id] - max - libm::logf(sum)
}
