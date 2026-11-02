//! `zoo-infer`: local language-model inference for ZooOS
//! (docs/m5b-design.md). Pure computation over a GGUF file in memory.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]
// Integer dot products in vectors (docs/a1-design.md): `core::simd` is
// safe code; the pinned nightly provides it.
#![feature(portable_simd)]

extern crate alloc;

pub mod assist;
pub mod chat;
pub mod gguf;
pub mod model;
pub mod quant;
pub mod tokenizer;

pub use gguf::{Gguf, GgufError};
pub use model::{Model, ModelError, State};
pub use tokenizer::Tokenizer;

/// Index of the largest value (first one on ties); greedy decoding.
pub fn argmax(xs: &[f32]) -> usize {
    let mut best = 0;
    for (i, &x) in xs.iter().enumerate() {
        if x > xs[best] {
            best = i;
        }
    }
    best
}
