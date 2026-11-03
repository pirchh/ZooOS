//! Bella answering in words (docs/talk-design.md): when no
//! file command fits a request, the model replies as Bella in a sentence or
//! two. The reply is only text: it is shown, never run. Like the tool
//! choice, the model sees only the request typed, never a file.
//!
//! The system prompt is fed once (`Talk::new`) and its key/value cache is
//! reused for every request by rewinding the position.

use crate::assist::{AssistError, BUDGET};
use crate::chat;
use crate::model::{Model, State};
use crate::tokenizer::Tokenizer;
use alloc::string::String;
use alloc::vec::Vec;

/// Who Bella is, for replies in words.
pub const SYSTEM: &str = "You are Bella, the assistant built into ZooOS, an operating system.\n\
You run entirely on this computer, with no internet.\n\
You can search, list and inspect the user's files and create folders when asked.\n\
Answer in one or two short, friendly sentences.";

/// Most tokens in a reply.
pub const MAX_REPLY_TOKENS: usize = 48;
/// Tokens the conversation may take after the system prompt; older turns
/// are forgotten when it is full (docs/talk-design.md §6).
pub const CONVERSATION_TOKENS: usize = 768;

/// One unit of progress, reported to the callback; returning `false`
/// cancels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TalkStep {
    /// A system-prompt token was fed (`done` of `total`).
    Prefix { done: usize, total: usize },
    /// A request token was fed.
    Request { done: usize, total: usize },
    /// A reply token was written.
    Reply { done: usize },
}

pub struct Talk<'m, 't> {
    model: &'m Model<'m>,
    tok: &'t Tokenizer<'t>,
    st: State,
    prefix_len: usize,
    /// Where the conversation so far ends in the cache: each reply and
    /// each noted exchange is kept, so later replies can refer to them.
    conv_end: usize,
}

impl<'m, 't> Talk<'m, 't> {
    /// Feeds the system prompt. `progress` may cancel (then `Cancelled`).
    pub fn new(
        model: &'m Model<'m>,
        tok: &'t Tokenizer<'t>,
        mut progress: impl FnMut(TalkStep) -> bool,
    ) -> Result<Self, AssistError> {
        let prefix = prefix_text();
        let ids = tok.encode(&prefix);
        let ctx = ids.len() + CONVERSATION_TOKENS;
        let mut st = model.new_state(ctx);
        for (k, &id) in ids.iter().enumerate() {
            if !progress(TalkStep::Prefix {
                done: k,
                total: ids.len(),
            }) {
                return Err(AssistError::Cancelled);
            }
            model.feed(&mut st, id)?;
        }
        Ok(Talk {
            model,
            tok,
            st,
            prefix_len: ids.len(),
            conv_end: ids.len(),
        })
    }

    pub fn prefix_tokens(&self) -> usize {
        self.prefix_len
    }

    /// Tokens of conversation kept now.
    pub fn conversation_tokens(&self) -> usize {
        self.conv_end - self.prefix_len
    }

    /// Room for `n` more tokens of conversation: when there is not, the
    /// conversation starts over after the system prompt.
    fn make_room(&mut self, n: usize) {
        if self.conv_end + n > self.prefix_len + CONVERSATION_TOKENS {
            self.conv_end = self.prefix_len;
        }
    }

    /// Adds an exchange that was not a reply in words (a command Bella ran
    /// and what it showed) to the conversation, so later replies know it.
    pub fn note(
        &mut self,
        user: &str,
        bella: &str,
        mut progress: impl FnMut(TalkStep) -> bool,
    ) -> Result<(), AssistError> {
        let mut text = String::from(user.trim());
        text.push_str("<|im_end|>\n<|im_start|>assistant\n");
        text.push_str(bella.trim());
        text.push_str("<|im_end|>\n<|im_start|>user\n");
        let ids = self.tok.encode(&text);
        if ids.len() > CONVERSATION_TOKENS / 3 {
            return Err(AssistError::TooLong);
        }
        self.make_room(ids.len());
        self.st.pos = self.conv_end;
        for (k, &id) in ids.iter().enumerate() {
            if !progress(TalkStep::Request {
                done: k,
                total: ids.len(),
            }) {
                // Half-fed: forget it rather than keep a broken turn.
                return Err(AssistError::Cancelled);
            }
            self.model.feed(&mut self.st, id)?;
        }
        self.conv_end = self.st.pos;
        Ok(())
    }

    /// Bella's reply to `request`: at most `MAX_REPLY_TOKENS`, one
    /// paragraph, printable characters only.
    pub fn reply(
        &mut self,
        request: &str,
        mut progress: impl FnMut(TalkStep) -> bool,
    ) -> Result<String, AssistError> {
        let request = request.trim();
        if request.is_empty() {
            return Err(AssistError::Empty);
        }
        if request.len() > BUDGET.max_request {
            return Err(AssistError::TooLong);
        }
        let mut text = String::from(request);
        text.push_str("<|im_end|>\n<|im_start|>assistant\n");
        let ids = self.tok.encode(&text);
        if ids.len() > BUDGET.max_request_tokens + 8 {
            return Err(AssistError::TooLong);
        }
        // The request and its reply continue the conversation.
        self.make_room(ids.len() + MAX_REPLY_TOKENS + 8);
        self.st.pos = self.conv_end;
        for (k, &id) in ids.iter().enumerate() {
            if !progress(TalkStep::Request {
                done: k,
                total: ids.len(),
            }) {
                return Err(AssistError::Cancelled);
            }
            if k + 1 == ids.len() {
                self.model.forward(&mut self.st, id)?;
            } else {
                self.model.feed(&mut self.st, id)?;
            }
        }
        let newline = self.tok.encode("\n");
        let mut out: Vec<u32> = Vec::new();
        for k in 0..MAX_REPLY_TOKENS {
            if !progress(TalkStep::Reply { done: k }) {
                return Err(AssistError::Cancelled);
            }
            let next = crate::argmax(&self.st.logits) as u32;
            // One paragraph: the turn's end, or a line break, ends it.
            if next == self.tok.eos || self.tok.is_control(next) || newline.contains(&next) {
                break;
            }
            out.push(next);
            self.model.forward(&mut self.st, next)?;
        }
        // Close the turn and open the next user turn, so the reply stays in
        // the conversation.
        let close = self.tok.encode("<|im_end|>\n<|im_start|>user\n");
        for &id in &close {
            self.model.feed(&mut self.st, id)?;
        }
        self.conv_end = self.st.pos;
        Ok(clean(&self.tok.decode(&out)))
    }
}

/// The reply as one line of printable characters (the window shows text
/// only; anything else becomes a space).
fn clean(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        out.push(if c.is_control() { ' ' } else { c });
    }
    out
}

/// The ChatML text before the user's request.
fn prefix_text() -> String {
    let full = chat::prompt(SYSTEM, "");
    let cut = full
        .rfind("<|im_start|>user\n")
        .map(|i| i + "<|im_start|>user\n".len())
        .unwrap_or(full.len());
    String::from(&full[..cut])
}
