//! Qwen2.5 chat formatting (ChatML) and greedy generation.

use crate::model::{Model, ModelError, State};
use crate::tokenizer::Tokenizer;
use alloc::string::String;
use alloc::vec::Vec;

/// `<|im_start|>role\ncontent<|im_end|>\n` for each turn, then the
/// assistant's opening, as Qwen2.5's chat template does without tools.
pub fn prompt(system: &str, user: &str) -> String {
    let mut s = String::new();
    for (role, content) in [("system", system), ("user", user)] {
        s.push_str("<|im_start|>");
        s.push_str(role);
        s.push('\n');
        s.push_str(content);
        s.push_str("<|im_end|>\n");
    }
    s.push_str("<|im_start|>assistant\n");
    s
}

/// The end of the user's turn and the opening of the model's. Qwen3's
/// opening carries an empty `<think></think>` block, as its chat template
/// writes with thinking off: the model answers at once instead of
/// reasoning first (docs/qwen3-design.md). Earlier turns kept in a
/// conversation are written without it, as the template does.
pub fn reply_open(model: &Model) -> &'static str {
    if model.cfg.qwen3 {
        "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
    } else {
        "<|im_end|>\n<|im_start|>assistant\n"
    }
}

/// `prompt`, with the model's own opening of its turn.
pub fn prompt_for(model: &Model, system: &str, user: &str) -> String {
    let mut s = prompt(system, user);
    if model.cfg.qwen3 {
        s.push_str("<think>\n\n</think>\n\n");
    }
    s
}

/// Feeds `prompt_ids`, then greedily generates up to `max_new` tokens,
/// stopping at the end-of-turn token. `on_token` sees each new token and may
/// return `false` to stop early (cancellation).
pub fn generate(
    model: &Model,
    tok: &Tokenizer,
    st: &mut State,
    prompt_ids: &[u32],
    max_new: usize,
    mut on_token: impl FnMut(u32) -> bool,
) -> Result<Vec<u32>, ModelError> {
    // Only the last prompt token's scores are read.
    for (k, &id) in prompt_ids.iter().enumerate() {
        if k + 1 == prompt_ids.len() {
            model.forward(st, id)?;
        } else {
            model.feed(st, id)?;
        }
    }
    let mut out = Vec::new();
    for _ in 0..max_new {
        let next = crate::argmax(&st.logits) as u32;
        if next == tok.eos || tok.is_control(next) {
            break;
        }
        out.push(next);
        if !on_token(next) {
            break;
        }
        model.forward(st, next)?;
    }
    Ok(out)
}
