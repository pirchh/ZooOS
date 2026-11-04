//! Speculative decoding with a draft model (docs/draft-design.md).
//!
//! A small model of the same family (the same tokenizer) guesses `k`
//! tokens; the large model checks them in one batch (`feed_checking`),
//! which reads each weight once for all of them, as reading a prompt does.
//! The guesses it agrees with are kept, then its own token where it
//! disagrees. The result is what greedy decoding with the large model
//! alone gives.

use crate::model::{Model, ModelError, State};
use alloc::vec::Vec;

/// What a run did, for measuring.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    /// Batches the large model checked.
    pub checks: usize,
    /// Guesses made, and kept.
    pub guessed: usize,
    pub kept: usize,
}

/// Greedy tokens after the context both states hold (at the same
/// position), starting with `next`, the large model's choice from it.
/// Stops before a token `stop` accepts, or after `max`. `k` guesses a check.
#[allow(clippy::too_many_arguments)]
pub fn speculate(
    big: &Model,
    bst: &mut State,
    draft: &Model,
    dst: &mut State,
    mut next: u32,
    k: usize,
    max: usize,
    stop: impl Fn(u32) -> bool,
) -> Result<(Vec<u32>, Stats), ModelError> {
    let k = k.max(1);
    let mut out = Vec::new();
    let mut stats = Stats::default();
    let mut guesses = Vec::with_capacity(k);
    let mut best = Vec::with_capacity(k + 1);
    let mut ids = Vec::with_capacity(k + 1);
    while out.len() < max && !stop(next) {
        let base = bst.pos;
        // The draft's guesses after `next`.
        guesses.clear();
        draft.forward(dst, next)?;
        for i in 0..k {
            let g = crate::argmax(&dst.logits) as u32;
            guesses.push(g);
            if i + 1 < k {
                draft.forward(dst, g)?;
            }
        }
        // The large model checks `next` and the guesses together.
        ids.clear();
        ids.push(next);
        ids.extend_from_slice(&guesses);
        big.feed_checking(bst, &ids, &mut best)?;
        stats.checks += 1;
        stats.guessed += k;
        out.push(next);
        let mut kept = 0;
        while kept < k && guesses[kept] == best[kept] && !stop(guesses[kept]) && out.len() < max {
            out.push(guesses[kept]);
            kept += 1;
        }
        stats.kept += kept;
        next = best[kept];
        // Both back to just after what was kept.
        bst.pos = base + 1 + kept;
        if kept == k {
            // The draft has not read its last guess yet.
            draft.feed(dst, guesses[k - 1])?;
        } else {
            dst.pos = base + 1 + kept;
        }
    }
    Ok((out, stats))
}
