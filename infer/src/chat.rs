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
    for &id in prompt_ids {
        model.forward(st, id)?;
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
