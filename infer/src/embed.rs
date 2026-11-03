//! Text embeddings with Qwen3-Embedding (docs/vectors-design.md): a text's
//! meaning as a unit-length vector, so texts about the same thing are
//! close even when they share no words.
//!
//! The model is a Qwen3 transformer without an output layer. A text gets
//! `<|endoftext|>` appended (the file's `add_eos_token`), and its vector
//! is the final hidden state at that token (`pooling_type` 3, "last"),
//! after the output norm, scaled to length 1. Queries get an instruction
//! first; documents do not, as the model card says.

use crate::model::{Model, ModelError, State};
use crate::tokenizer::Tokenizer;
use alloc::string::String;
use alloc::vec::Vec;

/// Longest text embedded, in tokens; longer texts are cut.
pub const MAX_TOKENS: usize = 256;
/// Dimensions kept: the model is trained so that the first ones carry the
/// most (Matryoshka); 256 of 1024 keeps an index small.
pub const DIM: usize = 256;
/// The instruction a question gets (the card's `Instruct: …\nQuery:`).
pub const QUERY_TASK: &str =
    "Given a question, retrieve lines from the user's own notes and files that answer it";

pub struct Embedder<'m, 't> {
    model: &'m Model<'m>,
    tok: &'t Tokenizer<'t>,
    st: State,
}

impl<'m, 't> Embedder<'m, 't> {
    pub fn new(model: &'m Model<'m>, tok: &'t Tokenizer<'t>) -> Self {
        Embedder {
            model,
            tok,
            st: model.new_state(MAX_TOKENS + 1),
        }
    }

    /// A document's vector (`DIM` long, length 1).
    pub fn document(&mut self, text: &str) -> Result<Vec<f32>, ModelError> {
        self.embed(text, DIM)
    }

    /// A question's vector, with the retrieval instruction.
    pub fn query(&mut self, question: &str) -> Result<Vec<f32>, ModelError> {
        let mut t = String::from("Instruct: ");
        t.push_str(QUERY_TASK);
        t.push_str("\nQuery:");
        t.push_str(question);
        self.embed(&t, DIM)
    }

    /// `text`'s vector cut to `dim` and scaled to length 1. `progress`
    /// is not offered: a line is a few hundred milliseconds at most.
    pub fn embed(&mut self, text: &str, dim: usize) -> Result<Vec<f32>, ModelError> {
        let mut ids = self.tok.encode(text);
        ids.truncate(MAX_TOKENS);
        ids.push(self.tok.eos);
        self.st.pos = 0;
        self.model.feed_many(&mut self.st, &ids)?;
        let mut v = self.model.final_hidden(&self.st);
        v.truncate(dim.min(v.len()));
        normalise(&mut v);
        Ok(v)
    }
}

/// Scales `v` to length 1 (a zero vector stays zero).
pub fn normalise(v: &mut [f32]) {
    let len = libm::sqrtf(v.iter().map(|x| x * x).sum());
    if len > 0.0 {
        for x in v.iter_mut() {
            *x /= len;
        }
    }
}

/// Cosine similarity of two unit vectors.
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_and_similarity() {
        let mut a = alloc::vec![3.0, 4.0];
        normalise(&mut a);
        assert!((a[0] - 0.6).abs() < 1e-6 && (a[1] - 0.8).abs() < 1e-6);
        assert!((similarity(&a, &a) - 1.0).abs() < 1e-6);
        let mut z = alloc::vec![0.0, 0.0];
        normalise(&mut z);
        assert_eq!(z, [0.0, 0.0]);
    }
}
