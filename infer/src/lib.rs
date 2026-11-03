//! `zoo-infer`: local language-model inference for ZooOS
//! (docs/m5b-design.md). Pure computation over a GGUF file in memory.

#![cfg_attr(not(test), no_std)]
// Safe code everywhere except the AVX2 dot products and the CPU check
// (docs/stage3-design.md): those modules, and the one call that chooses them,
// allow `unsafe`, each block with its assumptions.
#![deny(unsafe_code)]
// Integer dot products in vectors (docs/a1-design.md): `core::simd` is
// safe code; the pinned nightly provides it.
#![feature(portable_simd)]

extern crate alloc;

pub mod assist;
#[allow(unsafe_code)]
mod avx2;
pub mod chat;
#[allow(unsafe_code)]
pub mod cpu;
pub mod gguf;
pub mod model;
pub mod quant;
pub mod talk;
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
