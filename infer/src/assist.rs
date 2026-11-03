//! Guided tool choice for the assistant (docs/m5c2-design.md §3). The model
//! never writes free-form tool calls: it picks one of four tool words by
//! comparing next-token scores, then writes the argument with every token
//! restricted to path characters.
//!
//! The system prompt is fed once (`Bella::new`) and its key/value cache
//! reused for every request by rewinding the position.

use crate::chat;
use crate::model::{Model, ModelError, State};
use crate::tokenizer::Tokenizer;
use alloc::string::String;
use alloc::vec::Vec;

/// The instruction every request is answered under.
pub const SYSTEM: &str = "You turn a request about the user's files and apps into one command.\n\
Commands:\n\
search <words>: find where files are, by name or text\n\
list <folder>: show what is inside a folder\n\
inspect <file or folder>: how big it is\n\
create <folder>: make a folder\n\
read <file>: show a file's text\n\
open <file or folder>: open it in an app\n\
move <path> to <path>: move or rename it\n\
trash <file or folder>: delete it\n\
launch <app>: start an app (Files, Notes, Network)\n\
info: about this computer\n\
Paths are relative to the home folder. Reply with the command only.\n\
If no command fits the request, reply: none";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Search,
    List,
    Inspect,
    Create,
    /// Broad access (docs/access-design.md).
    Read,
    Open,
    Move,
    Trash,
    Launch,
    /// About the computer; no argument.
    Info,
    /// No command fits the request (docs/stage1-design.md, B9).
    Nothing,
}

pub const TOOLS: [Tool; 11] = [
    Tool::Search,
    Tool::List,
    Tool::Inspect,
    Tool::Create,
    Tool::Read,
    Tool::Open,
    Tool::Move,
    Tool::Trash,
    Tool::Launch,
    Tool::Info,
    Tool::Nothing,
];

impl Tool {
    pub const fn word(self) -> &'static str {
        match self {
            Tool::Search => "search",
            Tool::List => "list",
            Tool::Inspect => "inspect",
            Tool::Create => "create",
            Tool::Read => "read",
            Tool::Open => "open",
            Tool::Move => "move",
            Tool::Trash => "trash",
            Tool::Launch => "launch",
            Tool::Info => "info",
            Tool::Nothing => "none",
        }
    }
}

/// Limits for one request.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// Longest request accepted, in bytes.
    pub max_request: usize,
    /// Most tokens the request itself may take.
    pub max_request_tokens: usize,
    /// Most argument tokens generated.
    pub max_arg_tokens: usize,
}

/// Most bytes of context (the last exchange) given with a follow-up.
pub const MAX_CONTEXT: usize = 300;
/// Most tokens that context may take.
pub const MAX_CONTEXT_TOKENS: usize = 96;

pub const BUDGET: Budget = Budget {
    max_request: 200,
    max_request_tokens: 64,
    max_arg_tokens: 24,
};

#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub tool: Tool,
    pub arg: String,
    /// Score of the chosen tool word minus the runner-up's.
    pub margin: f32,
    pub request_tokens: usize,
    pub arg_tokens: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistError {
    Model(ModelError),
    /// The request is longer than the budget allows.
    TooLong,
    Empty,
    /// The progress callback asked to stop.
    Cancelled,
}

impl From<ModelError> for AssistError {
    fn from(e: ModelError) -> Self {
        AssistError::Model(e)
    }
}

/// Bytes an argument token may contain.
fn path_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'_' | b'-' | b'/')
}

pub struct Bella<'m, 't> {
    model: &'m Model<'m>,
    tok: &'t Tokenizer<'t>,
    pub st: State,
    /// Cache position after the system prompt.
    prefix_len: usize,
    /// First token of each tool word, in `TOOLS` order.
    tool_first: [u32; 11],
    /// Token ids of each tool word.
    tool_ids: [Vec<u32>; 11],
    /// `allowed[id]`: the token is made only of path bytes.
    allowed: Vec<bool>,
    newline: Vec<u32>,
}

/// One unit of progress, reported to the callback; returning `false`
/// cancels the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A system-prompt token was fed (`done` of `total`).
    Prefix { done: usize, total: usize },
    /// A request token was fed.
    Request { done: usize, total: usize },
    /// An argument token was generated.
    Arg { done: usize },
}

impl<'m, 't> Bella<'m, 't> {
    /// Feeds the system prompt. `progress` may cancel (then `Cancelled`).
    pub fn new(
        model: &'m Model<'m>,
        tok: &'t Tokenizer<'t>,
        mut progress: impl FnMut(Step) -> bool,
    ) -> Result<Self, AssistError> {
        let prefix = prefix_text();
        let prefix_ids = tok.encode(&prefix);
        // Prefix + request (+ template tokens) + tool word + argument.
        let ctx = prefix_ids.len()
            + MAX_CONTEXT_TOKENS
            + BUDGET.max_request_tokens
            + 16
            + BUDGET.max_arg_tokens;
        let mut st = model.new_state(ctx);
        for (k, &id) in prefix_ids.iter().enumerate() {
            if !progress(Step::Prefix {
                done: k,
                total: prefix_ids.len(),
            }) {
                return Err(AssistError::Cancelled);
            }
            // The prefix's scores are never read.
            model.feed(&mut st, id)?;
        }
        let tool_ids = TOOLS.map(|t| tok.encode(t.word()));
        let tool_first = core::array::from_fn(|i| tool_ids[i][0]);
        let mut bytes = Vec::new();
        let allowed = (0..tok.vocab_size() as u32)
            .map(|id| {
                if tok.is_control(id) {
                    return false;
                }
                bytes.clear();
                tok.token_bytes(id, &mut bytes);
                !bytes.is_empty() && bytes.iter().all(|&b| path_byte(b))
            })
            .collect();
        Ok(Bella {
            model,
            tok,
            st,
            prefix_len: prefix_ids.len(),
            tool_first,
            tool_ids,
            allowed,
            newline: tok.encode("\n"),
        })
    }

    pub fn prefix_tokens(&self) -> usize {
        self.prefix_len
    }

    /// Picks a tool and argument for `request`.
    pub fn choose(
        &mut self,
        request: &str,
        progress: impl FnMut(Step) -> bool,
    ) -> Result<Choice, AssistError> {
        self.choose_in(request, "", progress)
    }

    /// Picks a tool and argument for `request`, a follow-up to the
    /// exchange described in `context` (empty for none;
    /// docs/talk-design.md §6).
    pub fn choose_in(
        &mut self,
        request: &str,
        context: &str,
        mut progress: impl FnMut(Step) -> bool,
    ) -> Result<Choice, AssistError> {
        let request = request.trim();
        let context = context.trim();
        if context.len() > MAX_CONTEXT {
            return Err(AssistError::TooLong);
        }
        if request.is_empty() {
            return Err(AssistError::Empty);
        }
        if request.len() > BUDGET.max_request {
            return Err(AssistError::TooLong);
        }
        let mut text = String::new();
        if !context.is_empty() {
            text.push_str("Earlier: ");
            text.push_str(context);
            text.push_str("\nNow: ");
        }
        text.push_str(request);
        text.push_str("<|im_end|>\n<|im_start|>assistant\n");
        let ids = self.tok.encode(&text);
        if ids.len() > BUDGET.max_request_tokens + MAX_CONTEXT_TOKENS + 8 {
            return Err(AssistError::TooLong);
        }
        self.st.pos = self.prefix_len;
        for (k, &id) in ids.iter().enumerate() {
            if !progress(Step::Request {
                done: k,
                total: ids.len(),
            }) {
                return Err(AssistError::Cancelled);
            }
            // Only the last request token's scores are read.
            if k + 1 == ids.len() {
                self.model.forward(&mut self.st, id)?;
            } else {
                self.model.feed(&mut self.st, id)?;
            }
        }
        // Tool: best first-token score among the words.
        let scores = self.tool_first.map(|id| self.st.logits[id as usize]);
        let mut order: [usize; 11] = core::array::from_fn(|i| i);
        order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
        let best = order[0];
        let margin = scores[best] - scores[order[1]];
        let tool = TOOLS[best];
        if tool == Tool::Nothing || tool == Tool::Info {
            return Ok(Choice {
                tool,
                arg: String::new(),
                margin,
                request_tokens: ids.len(),
                arg_tokens: 0,
            });
        }
        let word = self.tool_ids[best].clone();
        for (k, &id) in word.iter().enumerate() {
            if k + 1 == word.len() {
                self.model.forward(&mut self.st, id)?;
            } else {
                self.model.feed(&mut self.st, id)?;
            }
        }
        // Argument: path characters only, until the model would rather end
        // the line or the turn.
        let mut arg_ids = Vec::new();
        for k in 0..BUDGET.max_arg_tokens {
            if !progress(Step::Arg { done: k }) {
                return Err(AssistError::Cancelled);
            }
            let free = crate::argmax(&self.st.logits) as u32;
            if free == self.tok.eos || self.tok.is_control(free) || self.newline.contains(&free) {
                break;
            }
            let next = self
                .st
                .logits
                .iter()
                .enumerate()
                .filter(|&(id, _)| self.allowed.get(id) == Some(&true))
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(id, _)| id as u32);
            let Some(next) = next else { break };
            arg_ids.push(next);
            self.model.forward(&mut self.st, next)?;
        }
        let arg = String::from(self.tok.decode(&arg_ids).trim());
        Ok(Choice {
            tool,
            arg,
            margin,
            request_tokens: ids.len(),
            arg_tokens: arg_ids.len(),
        })
    }
}

/// The ChatML text before the user's request.
pub fn prefix_text() -> String {
    let full = chat::prompt(SYSTEM, "");
    // `prompt` ends with "<|im_end|>\n<|im_start|>assistant\n" after the
    // (empty) user content; keep everything up to the user content.
    let cut = full
        .rfind("<|im_start|>user\n")
        .map(|i| i + "<|im_start|>user\n".len())
        .unwrap_or(full.len());
    String::from(&full[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_stops_where_the_request_goes() {
        let p = prefix_text();
        assert!(p.starts_with("<|im_start|>system\n"));
        assert!(p.ends_with("<|im_end|>\n<|im_start|>user\n"));
        assert!(p.contains(SYSTEM));
    }

    #[test]
    fn path_bytes() {
        for b in b"Projects/2026 my-notes_v1.txt" {
            assert!(path_byte(*b));
        }
        for b in b"\"'`*?<>|:\n\\" {
            assert!(!path_byte(*b));
        }
    }
}
