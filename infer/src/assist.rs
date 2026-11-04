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

/// Qwen3's version (docs/qwen3-design.md): the same commands, with
/// examples (none worded like `assist-check`'s requests), a narrower
/// "none", and the window actions (docs/window-actions-design.md). Qwen3 chose `none` for plain file requests with `SYSTEM`; the
/// 0.5B model copies the examples, so it keeps `SYSTEM`.
pub const SYSTEM_QWEN3: &str = "You turn a request about the user's files and apps into one command.\n\
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
maximize <app>: open an app and make it fill the screen\n\
minimize <app>: hide an app's window in the dock\n\
close <app>: quit an app\n\
split <apps>: tile windows side by side\n\
info: this computer's storage and memory\n\
Paths are relative to the home folder. Reply with the command only.\n\
Examples:\n\
Where did I put my budget? -> search budget\n\
Show the Videos folder -> list Videos\n\
Open my holiday letter -> open holiday letter\n\
How big is Downloads? -> inspect Downloads\n\
Write me a poem -> none\n\
Reply none only when the request is not about the user's files, folders, apps or this computer.";

/// The apps `launch`, `maximize`, `minimize` and `close` can name (the
/// list in the prompts). Any other name is no command: there is no such
/// app.
pub const LAUNCHABLE: [&str; 3] = ["Files", "Notes", "Network"];

/// The system prompt for `model`.
pub fn system_for(model: &Model) -> &'static str {
    if model.cfg.qwen3 {
        SYSTEM_QWEN3
    } else {
        SYSTEM
    }
}

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
    /// Window actions on an app (docs/window-actions-design.md).
    Maximize,
    Minimize,
    Close,
    /// Two apps side by side (docs/tiling-design.md).
    Split,
    /// About the computer; no argument.
    Info,
    /// No command fits the request (docs/stage1-design.md, B9).
    Nothing,
}

pub const TOOLS: [Tool; 15] = [
    Tool::Search,
    Tool::List,
    Tool::Inspect,
    Tool::Create,
    Tool::Read,
    Tool::Open,
    Tool::Move,
    Tool::Trash,
    Tool::Launch,
    Tool::Maximize,
    Tool::Minimize,
    Tool::Close,
    Tool::Split,
    Tool::Info,
    Tool::Nothing,
];

impl Tool {
    /// `maximize`, `minimize` or `close`.
    pub const fn is_window(self) -> bool {
        matches!(
            self,
            Tool::Maximize | Tool::Minimize | Tool::Close | Tool::Split
        )
    }

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
            Tool::Maximize => "maximize",
            Tool::Minimize => "minimize",
            Tool::Close => "close",
            Tool::Split => "split",
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
    tool_first: [u32; TOOLS.len()],
    /// Token ids of each tool word.
    tool_ids: [Vec<u32>; TOOLS.len()],
    /// `allowed[id]`: the token is made only of path bytes.
    allowed: Vec<bool>,
    /// Whether the prompt lists the window actions: Qwen3's does; with
    /// them, 0.5B acted on 4 of 8 requests that have no command
    /// (docs/window-actions-design.md), so its prompt does not.
    windows: bool,
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
        let prefix = prefix_text_with(system_for(model));
        let prefix_ids = tok.encode(&prefix);
        // Prefix + request (+ template tokens) + tool word + argument.
        let ctx = prefix_ids.len()
            + MAX_CONTEXT_TOKENS
            + BUDGET.max_request_tokens
            + 16
            + BUDGET.max_arg_tokens;
        let mut st = model.new_state(ctx);
        // The prefix's scores are never read.
        let total = prefix_ids.len();
        if !model.read_prompt(&mut st, &prefix_ids, false, |done| {
            progress(Step::Prefix { done, total })
        })? {
            return Err(AssistError::Cancelled);
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
            windows: model.cfg.qwen3,
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
        text.push_str(chat::reply_open(self.model));
        let ids = self.tok.encode(&text);
        // 16: the turn's opening, with Qwen3's empty think block.
        if ids.len() > BUDGET.max_request_tokens + MAX_CONTEXT_TOKENS + 16 {
            return Err(AssistError::TooLong);
        }
        self.st.pos = self.prefix_len;
        // Only the last request token's scores are read.
        let total = ids.len();
        if !self.model.read_prompt(&mut self.st, &ids, true, |done| {
            progress(Step::Request { done, total })
        })? {
            return Err(AssistError::Cancelled);
        }
        // Tool: best first-token score among the words.
        let mut scores = self.tool_first.map(|id| self.st.logits[id as usize]);
        if !self.windows {
            for (k, t) in TOOLS.iter().enumerate() {
                if t.is_window() {
                    scores[k] = f32::NEG_INFINITY;
                }
            }
        }
        let mut order: [usize; TOOLS.len()] = core::array::from_fn(|i| i);
        order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
        let best = order[0];
        let margin = scores[best] - scores[order[1]];
        let tool = TOOLS[best];
        // `split`: the model decides it is a split; which apps, in which
        // order, comes from the request's own words (the model's argument
        // put them in the prompt's order; docs/tiling-design.md).
        if tool == Tool::Split {
            return Ok(match split_request(request) {
                Some(arg) => Choice {
                    tool,
                    arg,
                    margin,
                    request_tokens: ids.len(),
                    arg_tokens: 0,
                },
                None => Choice {
                    tool: Tool::Nothing,
                    arg: String::new(),
                    margin,
                    request_tokens: ids.len(),
                    arg_tokens: 0,
                },
            });
        }
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
        // No such app: no command (the model named one the prompt does not
        // list, e.g. "Email" for "Send an email to my boss").
        // "the Notes app" names Notes.
        let app = LAUNCHABLE.iter().find(|a| {
            let name = arg.strip_prefix("the ").unwrap_or(&arg);
            let name = name.strip_suffix(" app").unwrap_or(name);
            a.eq_ignore_ascii_case(name.trim())
        });
        let names_app = matches!(
            tool,
            Tool::Launch | Tool::Maximize | Tool::Minimize | Tool::Close
        );
        // The apps the request itself names win over the model's argument
        // (it wrote them in its prompt's order for splits;
        // docs/multi-step-design.md): "open Notes and Files" launches both.
        let said = apps_named(request, false);
        // "open Notes and Files": apps, not a file called that.
        let (tool, names_app) = if tool == Tool::Open && said.len() >= 2 {
            (Tool::Launch, true)
        } else {
            (tool, names_app)
        };
        if names_app && !said.is_empty() {
            let arg = if tool == Tool::Maximize {
                String::from(said[0])
            } else {
                said.join(" and ")
            };
            return Ok(Choice {
                tool,
                arg,
                margin,
                request_tokens: ids.len(),
                arg_tokens: arg_ids.len(),
            });
        }
        let (tool, arg) = match (names_app, app) {
            (true, Some(a)) => (tool, String::from(*a)),
            (true, None) => (Tool::Nothing, String::new()),
            _ if tool == Tool::Split => match split_apps(&arg) {
                Some(a) => (tool, a),
                None => (Tool::Nothing, String::new()),
            },
            _ => (tool, arg),
        };
        Ok(Choice {
            tool,
            arg,
            margin,
            request_tokens: ids.len(),
            arg_tokens: arg_ids.len(),
        })
    }
}

/// `split`'s argument with its app names checked: "Notes and Bella",
/// "Files and Files" (two of its windows), one name, or "" (no app named:
/// the two front windows). The names are taken in order from the words;
/// more than three is no command (docs/tiling-design.md).
pub fn split_apps(arg: &str) -> Option<String> {
    let mut apps: Vec<&str> = Vec::new();
    for word in arg.split(|c: char| !c.is_ascii_alphanumeric()) {
        let app = if ["bella", "assistant", "you", "yourself"]
            .iter()
            .any(|b| b.eq_ignore_ascii_case(word))
        {
            "Bella"
        } else if word.eq_ignore_ascii_case("finder") {
            "Files"
        } else if let Some(a) = LAUNCHABLE.iter().find(|a| a.eq_ignore_ascii_case(word)) {
            a
        } else {
            continue;
        };
        apps.push(app);
    }
    (apps.len() <= 3).then(|| apps.join(" and "))
}

/// The apps `text` names, in order: Files (or "Finder"), Notes, Network,
/// and with `bella` also Bella ("assistant", "yourself", or "Bella" after
/// the first two words). Each once (docs/multi-step-design.md).
pub fn apps_named(text: &str, bella: bool) -> Vec<&'static str> {
    let mut apps: Vec<&'static str> = Vec::new();
    for (k, w) in text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .enumerate()
    {
        let is = |s: &str| w.eq_ignore_ascii_case(s);
        let app = if bella && (is("assistant") || is("yourself") || (is("bella") && k >= 2)) {
            Some("Bella")
        } else if is("finder") {
            Some("Files")
        } else {
            LAUNCHABLE.iter().copied().find(|a| is(a))
        };
        if let Some(a) = app
            && !apps.contains(&a)
        {
            apps.push(a);
        }
    }
    apps
}

/// A later step with "it", "them" or "that" naming the apps of the step
/// before ("open Notes" then "maximize it" → "maximize Notes"); unchanged
/// when the step before named none (docs/multi-step-design.md).
pub fn resolve_it(step: &str, before: &str) -> String {
    let apps = apps_named(before, false);
    if apps.is_empty() {
        return String::from(step);
    }
    let names = apps.join(" and ");
    let mut out = String::new();
    for (k, w) in step.split(' ').enumerate() {
        if k > 0 {
            out.push(' ');
        }
        let bare = w.trim_end_matches([',', '.', '?', '!']);
        if ["it", "them", "that", "both"]
            .iter()
            .any(|p| p.eq_ignore_ascii_case(bare))
        {
            out.push_str(&names);
            out.push_str(&w[bare.len()..]);
        } else {
            out.push_str(w);
        }
    }
    out
}

/// The apps a split request names, left to right, as `split`'s argument
/// ("Notes and Bella"; "" for none: the two front windows). Words after
/// an app up to the next one may give its side ("Files on the right and
/// Notes on the left"); otherwise the order spoken is kept. Bella counts
/// as "assistant", "yourself", or "Bella" after the first two words (not
/// "hey Bella, ..."); "you" does not ("can you split ..."). More than
/// three apps: `None` (docs/tiling-design.md).
pub fn split_request(request: &str) -> Option<String> {
    let words: Vec<&str> = request
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    // (app, side: 0 left, 1 middle, 2 right, 3 unsaid)
    let mut apps: Vec<(&str, u8)> = Vec::new();
    // "two Files windows": the next app, this many times.
    let mut times = 1;
    for (k, w) in words.iter().enumerate() {
        let is = |s: &str| w.eq_ignore_ascii_case(s);
        let count = if is("two") || is("2") || is("both") {
            2
        } else if is("three") || is("3") {
            3
        } else {
            0
        };
        if count > 0 {
            times = count;
            continue;
        }
        let app = if is("assistant") || is("yourself") || (is("bella") && k >= 2) {
            Some("Bella")
        } else if is("finder") {
            Some("Files")
        } else {
            LAUNCHABLE.iter().copied().find(|a| is(a))
        };
        if let Some(a) = app {
            if times > 1 {
                for _ in 0..times {
                    apps.push((a, 3));
                }
            } else if !apps.iter().any(|x| x.0 == a) {
                apps.push((a, 3));
            }
            times = 1;
            continue;
        }
        if let Some(last) = apps.last_mut()
            && last.1 == 3
        {
            last.1 = if is("left") {
                0
            } else if is("middle") || is("centre") || is("center") {
                1
            } else if is("right") {
                2
            } else {
                3
            };
        }
    }
    if apps.len() > 3 {
        return None;
    }
    // Every app given a side: by side; else as spoken.
    if apps.len() > 1 && apps.iter().all(|a| a.1 != 3) {
        apps.sort_by_key(|a| a.1);
    }
    let names: Vec<&str> = apps.iter().map(|a| a.0).collect();
    Some(names.join(" and "))
}

/// The ChatML text before the user's request, with `system`.
pub fn prefix_text_with(system: &str) -> String {
    let full = chat::prompt(system, "");
    // `prompt` ends with "<|im_end|>\n<|im_start|>assistant\n" after the
    // (empty) user content; keep everything up to the user content.
    let cut = full
        .rfind("<|im_start|>user\n")
        .map(|i| i + "<|im_start|>user\n".len())
        .unwrap_or(full.len());
    String::from(&full[..cut])
}

/// `prefix_text_with` the 0.5B prompt.
pub fn prefix_text() -> String {
    prefix_text_with(SYSTEM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_names_the_apps_before() {
        assert_eq!(resolve_it("maximize it", "open Notes"), "maximize Notes");
        assert_eq!(
            resolve_it("put them side by side", "open notes and files"),
            "put Notes and Files side by side"
        );
        assert_eq!(resolve_it("open it", "find my pancake recipe"), "open it");
        assert_eq!(apps_named("hey bella open the finder", true), ["Files"]);
    }

    #[test]
    fn split_reads_the_request() {
        let s = |r: &str| split_request(r);
        assert_eq!(
            s("Put notes on the left and the assistant on the right").as_deref(),
            Some("Notes and Bella")
        );
        assert_eq!(
            s("put Files on the right and Notes on the left").as_deref(),
            Some("Notes and Files")
        );
        assert_eq!(
            s("I want Notes and Files next to each other").as_deref(),
            Some("Notes and Files")
        );
        assert_eq!(s("hey bella split these 2 windows").as_deref(), Some(""));
        assert_eq!(
            s("can you split the screen with the files, notes and network apps").as_deref(),
            Some("Files and Notes and Network")
        );
        assert_eq!(s("Bella, tile my windows").as_deref(), Some(""));
        assert_eq!(s("files notes network and the assistant"), None);
        assert_eq!(
            s("put two Files windows side by side").as_deref(),
            Some("Files and Files")
        );
        assert_eq!(
            s("split 2 finder windows and notes").as_deref(),
            Some("Files and Files and Notes")
        );
    }

    #[test]
    fn split_names_apps() {
        assert_eq!(
            split_apps("Notes and Bella").as_deref(),
            Some("Notes and Bella")
        );
        assert_eq!(
            split_apps("the notes app and the assistant").as_deref(),
            Some("Notes and Bella")
        );
        assert_eq!(split_apps("Files").as_deref(), Some("Files"));
        assert_eq!(
            split_apps("Files and Notes side by side").as_deref(),
            Some("Files and Notes")
        );
        assert_eq!(split_apps("Notes and Pizza").as_deref(), Some("Notes"));
        assert_eq!(split_apps("Pizza").as_deref(), Some(""));
        assert_eq!(split_apps("these two windows").as_deref(), Some(""));
        assert_eq!(
            split_apps("Files, Notes and Network").as_deref(),
            Some("Files and Notes and Network")
        );
        assert_eq!(split_apps("Notes Files Network Bella"), None);
    }

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
