//! Guided tool choice for the assistant (docs/m5c2-design.md §3). The model
//! never writes free-form tool calls: it picks one of four tool words by
//! comparing next-token scores, then writes the argument with every token
//! restricted to path characters.
//!
//! The system prompt is fed once (`Bella::new`) and its key/value cache
//! reused for every request by rewinding the position.

use crate::chat;
use crate::model::{Model, ModelError, State};
use crate::registry::Registry;
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
pub const SYSTEM_QWEN3_HEAD: &str = "You turn a request about the user's files and apps into one command.\n\
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
split <apps>: tile windows side by side\n";

/// Qwen3's prompt: the system's own commands, then every registered app
/// action's line (docs/registry-design.md), then the rest.
pub fn system_qwen3(reg: &Registry) -> String {
    let mut s = String::from(SYSTEM_QWEN3_HEAD);
    for a in &reg.actions {
        s.push_str(&a.prompt_line());
        s.push('\n');
    }
    s.push_str(SYSTEM_QWEN3_TAIL);
    s
}

/// The prompt after the registry's lines.
pub const SYSTEM_QWEN3_TAIL: &str = "info: this computer's storage and memory\n\
Paths are relative to the home folder. Reply with the command only.\n\
Examples:\n\
Where did I put my budget? -> search budget\n\
Show the Videos folder -> list Videos\n\
Open my holiday letter -> open holiday letter\n\
How big is Downloads? -> inspect Downloads\n\
Write me a poem -> none\n\
Set an alarm for seven -> none\n\
Reply none only when the request is not about the user's files, folders, apps or this computer.";

/// The apps `launch`, `maximize`, `minimize` and `close` can name (the
/// list in the prompts). Any other name is no command: there is no such
/// app.
pub const LAUNCHABLE: [&str; 3] = ["Files", "Notes", "Network"];

/// The system prompt for `model`; Qwen3's lists the registry's actions.
pub fn system_for(model: &Model, reg: &Registry) -> String {
    if model.cfg.qwen3 {
        system_qwen3(reg)
    } else {
        String::from(SYSTEM)
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
    /// A registered app action (docs/registry-design.md): its index in the
    /// registry.
    App(usize),
    /// An app's window to a monitor; never chosen by the model's scores,
    /// only when the request names a monitor (docs/monitors-design.md).
    Show,
    /// About the computer; no argument.
    Info,
    /// Windows arranged by a layout tree (docs/layouts-design.md), never
    /// chosen by the model's scores: read from the request's words.
    Layout,
    /// Words instead of an action, never chosen by the model's scores:
    /// what cannot be done yet, or a question for what is missing
    /// (docs/one-bella-design.md).
    Say,
    /// No command fits the request (docs/stage1-design.md, B9).
    Nothing,
}

/// The system's own commands; registered app actions follow them.
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
            Tool::Maximize | Tool::Minimize | Tool::Close | Tool::Split | Tool::App(_)
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
            // Its word is in the registry (`Choice::word`).
            Tool::App(_) => "app",
            Tool::Show => "show",
            Tool::Info => "info",
            Tool::Say => "say",
            Tool::Layout => "layout",
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
    /// The command word: the tool's, or a registered action's.
    pub word: String,
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
    /// The commands the model chooses among: `TOOLS`, then the registry's
    /// actions (Qwen3 only).
    tools: Vec<Tool>,
    /// First token of each tool word, in `tools` order.
    tool_first: Vec<u32>,
    /// Token ids of each tool word.
    tool_ids: Vec<Vec<u32>>,
    /// The app actions (docs/registry-design.md).
    registry: Registry,
    /// Registered actions left out, and why (a word whose first token
    /// another command already starts with).
    pub skipped: Vec<String>,
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
        registry: Registry,
        mut progress: impl FnMut(Step) -> bool,
    ) -> Result<Self, AssistError> {
        // The model chooses by first token: an action whose word starts
        // like another command's is left out (and out of the prompt).
        let mut tools: Vec<Tool> = TOOLS.to_vec();
        let mut tool_ids: Vec<Vec<u32>> = TOOLS.iter().map(|t| tok.encode(t.word())).collect();
        let mut kept = Registry::default();
        let mut skipped = Vec::new();
        if model.cfg.qwen3 {
            for a in registry.actions {
                let ids = tok.encode(&a.word);
                match tool_ids.iter().position(|t| t.first() == ids.first()) {
                    Some(k) if !ids.is_empty() => skipped.push(alloc::format!(
                        "{}.{}: `{}` starts like `{}`",
                        a.program,
                        a.name,
                        a.word,
                        if k < TOOLS.len() {
                            String::from(TOOLS[k].word())
                        } else {
                            kept.actions[k - TOOLS.len()].word.clone()
                        }
                    )),
                    _ if ids.is_empty() => {
                        skipped.push(alloc::format!("{}.{}: no word", a.program, a.name))
                    }
                    _ => {
                        tools.push(Tool::App(kept.actions.len()));
                        tool_ids.push(ids);
                        kept.actions.push(a);
                    }
                }
            }
        }
        let registry = kept;
        let prefix = prefix_text_with(&system_for(model, &registry));
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
        // Every request starts from here (a recurrent model's state too).
        st.keep(0);
        let tool_first = tool_ids.iter().map(|t| t[0]).collect();
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
            tools,
            tool_first,
            tool_ids,
            registry,
            skipped,
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
        progress: impl FnMut(Step) -> bool,
    ) -> Result<Choice, AssistError> {
        let mut c = self.choose_inner(request, context, progress)?;
        c.word = String::from(self.word_of(c.tool));
        Ok(c)
    }

    /// The command word of `tool`.
    pub fn word_of(&self, tool: Tool) -> &str {
        match tool {
            Tool::App(i) => &self.registry.actions[i].word,
            t => t.word(),
        }
    }

    /// The registered actions the model can choose.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    fn choose_inner(
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
        self.st.rewind(self.prefix_len)?;
        // Only the last request token's scores are read.
        let total = ids.len();
        if !self.model.read_prompt(&mut self.st, &ids, true, |done| {
            progress(Step::Request { done, total })
        })? {
            return Err(AssistError::Cancelled);
        }
        // Tool: best first-token score among the words.
        let mut scores: Vec<f32> = self
            .tool_first
            .iter()
            .map(|&id| self.st.logits[id as usize])
            .collect();
        if !self.windows {
            for (k, t) in self.tools.iter().enumerate() {
                if t.is_window() {
                    scores[k] = f32::NEG_INFINITY;
                }
            }
        }
        let mut order: Vec<usize> = (0..self.tools.len()).collect();
        order.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));
        let best = order[0];
        let margin = scores[best] - scores[order[1]];
        let tool = self.tools[best];
        // A monitor named with an app: the window goes there, filled when
        // the request says so, split when the model chose a split, else
        // as it is (docs/monitors-design.md).
        if self.windows
            && let Some(m) = monitor_named(request)
        {
            let apps = apps_named(request, true);
            if !apps.is_empty() {
                let lower = request.to_ascii_lowercase();
                let fill = [
                    "maximize",
                    "maximise",
                    "maximized",
                    "maximised",
                    "full screen",
                    "fullscreen",
                    "fill",
                ]
                .iter()
                .any(|w| lower.contains(w));
                let on = if m == "next" {
                    String::from(" on the next monitor")
                } else {
                    alloc::format!(" on monitor {m}")
                };
                let (tool, arg) = if fill {
                    (Tool::Maximize, alloc::format!("{}{on}", apps[0]))
                } else if tool == Tool::Split && apps.len() > 1 {
                    (Tool::Split, alloc::format!("{}{on}", apps.join(" and ")))
                } else {
                    (Tool::Show, alloc::format!("{}{on}", apps.join(" and ")))
                };
                return Ok(Choice {
                    word: String::new(),
                    tool,
                    arg,
                    margin,
                    request_tokens: ids.len(),
                    arg_tokens: 0,
                });
            }
        }
        // A registered action: its slots come from the request's own words
        // (docs/registry-design.md); the model writes a single text slot
        // only when none is found. An action the request cannot fill (no
        // address for `browse`, no "to" for `change`) is not a fit: no
        // command ("what's the weather today?" chose `browse`).
        if let Tool::App(i) = tool {
            let action = &self.registry.actions[i];
            let own = action.fill(request);
            let model_may_write = action.slots.len() == 1 && action.slots[0] != "address";
            if !own.is_empty() || !model_may_write {
                return Ok(Choice {
                    word: String::new(),
                    tool: if own.is_empty() { Tool::Nothing } else { tool },
                    arg: own,
                    margin,
                    request_tokens: ids.len(),
                    arg_tokens: 0,
                });
            }
        }
        // `split`: the model decides it is a split; which apps, in which
        // order, comes from the request's own words (the model's argument
        // put them in the prompt's order; docs/tiling-design.md).
        // A layout the words describe (rows, quarters, "two on top and one
        // below"): the window server's general arrangement
        // (docs/layouts-design.md), when the model chose a split or the
        // request asks for an arrangement.
        if self.windows && (tool == Tool::Split || arranges(request)) {
            let named = split_request(request).unwrap_or_default();
            let apps: Vec<&str> = named.split(" and ").filter(|a| !a.is_empty()).collect();
            if let Some(tree) = crate::layout::from_words(request, &apps) {
                return Ok(Choice {
                    word: String::new(),
                    tool: Tool::Layout,
                    arg: tree.text(),
                    margin,
                    request_tokens: ids.len(),
                    arg_tokens: 0,
                });
            }
        }
        if tool == Tool::Split {
            // "three ways" with fewer apps named: ask which.
            if let Some(n) = split_ways(request) {
                let named = split_request(request).unwrap_or_default();
                let count = if named.is_empty() {
                    0
                } else {
                    named.split(" and ").count()
                };
                if count < n && !(count == 0 && n == 2) {
                    let heard = if named.is_empty() {
                        String::new()
                    } else {
                        alloc::format!(" I heard {named}.")
                    };
                    return Ok(Choice {
                        word: String::new(),
                        tool: Tool::Say,
                        arg: alloc::format!("Which {n} apps should I put side by side?{heard}"),
                        margin,
                        request_tokens: ids.len(),
                        arg_tokens: 0,
                    });
                }
            }
            return Ok(match split_request(request) {
                Some(arg) => Choice {
                    word: String::new(),
                    tool,
                    arg,
                    margin,
                    request_tokens: ids.len(),
                    arg_tokens: 0,
                },
                None => Choice {
                    word: String::new(),
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
                word: String::new(),
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
                word: String::new(),
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
            word: String::new(),
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
        // Bella is one (docs/one-bella-design.md).
        if app == "Bella" && apps.contains(&"Bella") {
            continue;
        }
        apps.push(app);
    }
    (apps.len() <= 4).then(|| apps.join(" and "))
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

/// The web address a request names: a word with a dot in it
/// ("example.com", "10.0.2.100", "http://example.com/"), or "localhost".
pub fn address_in(request: &str) -> Option<&str> {
    request
        .split_whitespace()
        .map(|w| {
            w.trim_end_matches([',', '?', '!', ')'])
                .trim_end_matches('.')
        })
        .find(|w| {
            let host = w
                .strip_prefix("http://")
                .unwrap_or(w)
                .split('/')
                .next()
                .unwrap_or("");
            host.eq_ignore_ascii_case("localhost")
                || (host.contains('.')
                    && !host.starts_with('.')
                    && host
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':')))
        })
}

/// The monitor a request names: "monitor 2", "screen three", "the second
/// display", "3rd monitor" as `"2"`.. ; "the other monitor" or "the next
/// screen" as `"next"` (docs/monitors-design.md).
pub fn monitor_named(text: &str) -> Option<&'static str> {
    const NUMBERS: [(&str, &str); 12] = [
        ("1", "1"),
        ("one", "1"),
        ("2", "2"),
        ("two", "2"),
        ("3", "3"),
        ("three", "3"),
        ("4", "4"),
        ("four", "4"),
        ("first", "1"),
        ("second", "2"),
        ("third", "3"),
        ("fourth", "4"),
    ];
    let words: Vec<String> = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let is_monitor = |w: &str| matches!(w, "monitor" | "screen" | "display");
    let number = |w: &str| {
        let w = w
            .strip_suffix("st")
            .or_else(|| w.strip_suffix("nd"))
            .or_else(|| w.strip_suffix("rd"))
            .or_else(|| w.strip_suffix("th"))
            .filter(|d| d.chars().all(|c| c.is_ascii_digit()) && !d.is_empty())
            .unwrap_or(w);
        NUMBERS.iter().find(|(k, _)| *k == w).map(|(_, v)| *v)
    };
    for (k, w) in words.iter().enumerate() {
        if !is_monitor(w) {
            continue;
        }
        // "monitor 2"
        if let Some(n) = words.get(k + 1).and_then(|n| number(n)) {
            return Some(n);
        }
        // "the second monitor", "the other screen"
        if k > 0 {
            let before = words[k - 1].as_str();
            if let Some(n) = number(before) {
                return Some(n);
            }
            if matches!(before, "other" | "next") {
                return Some("next");
            }
        }
    }
    None
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
        } else if is("four") || is("4") {
            4
        } else {
            0
        };
        // "three ways" counts columns (`split_ways`), not copies of the
        // next app named: "split it three ways with Bella" made three
        // Bellas (docs/one-bella-design.md).
        let ways = words
            .get(k + 1)
            .is_some_and(|n| WAYS.iter().any(|w| n.eq_ignore_ascii_case(w)));
        if count > 0 && !ways {
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
            // Bella is one: never two columns of her.
            if times > 1 && a != "Bella" {
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
    if apps.len() > 4 {
        return None;
    }
    // Every app given a side: by side; else as spoken.
    if apps.len() > 1 && apps.iter().all(|a| a.1 != 3) {
        apps.sort_by_key(|a| a.1);
    }
    let names: Vec<&str> = apps.iter().map(|a| a.0).collect();
    Some(names.join(" and "))
}

/// Words after a number that make it a count of columns ("three ways").
const WAYS: [&str; 5] = ["ways", "way", "columns", "parts", "pieces"];

/// The columns a split request asks for ("three ways", "4 columns").
pub fn split_ways(request: &str) -> Option<usize> {
    let words: Vec<&str> = request
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words.windows(2).find_map(|p| {
        if !WAYS.iter().any(|w| p[1].eq_ignore_ascii_case(w)) {
            return None;
        }
        ["two", "three", "four"]
            .iter()
            .position(|n| p[0].eq_ignore_ascii_case(n))
            .map(|k| k + 2)
            .or_else(|| p[0].parse().ok().filter(|n| (2..=4).contains(n)))
    })
}

/// Whether a request asks for windows to be arranged ("split", "stack",
/// "arrange", "tile", "lay out", "organise", "put ... on top").
pub fn arranges(request: &str) -> bool {
    const VERBS: [&str; 11] = [
        "split", "stack", "arrange", "tile", "lay", "layout", "organise", "organize", "divide",
        "put", "set",
    ];
    request
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| VERBS.iter().any(|v| w.eq_ignore_ascii_case(v)))
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
    fn notes_and_addresses() {
        assert_eq!(address_in("go to example.com please"), Some("example.com"));
        assert_eq!(
            address_in("open the page at 10.0.2.100"),
            Some("10.0.2.100")
        );
        assert_eq!(
            address_in("browse http://example.org/news."),
            Some("http://example.org/news")
        );
        assert_eq!(address_in("open my notes"), None);
    }

    #[test]
    fn splits_from_the_owner_demo() {
        // "three ways" is columns, and Bella is one (docs/one-bella-design.md).
        assert_eq!(
            split_request("split it three ways with Bella's the third window").unwrap(),
            "Bella"
        );
        assert_eq!(
            split_ways("split it three ways with Bella's the third window"),
            Some(3)
        );
        assert_eq!(split_request("split two bella windows").unwrap(), "Bella");
        assert_eq!(
            split_request("put two finder windows side by side").unwrap(),
            "Files and Files"
        );
        assert_eq!(
            split_request("split two files windows, notes and network").unwrap(),
            "Files and Files and Notes and Network"
        );
        assert_eq!(split_ways("split the screen 4 ways"), Some(4));
        assert_eq!(split_ways("split these two windows"), None);
        assert!(arranges(
            "split it three ways, two on top and one on the bottom"
        ));
        assert!(arranges("stack notes and files"));
        assert!(!arranges("what is in my notes"));
    }

    #[test]
    fn monitors_named() {
        assert_eq!(
            monitor_named("put notes maximized on monitor 3 please"),
            Some("3")
        );
        assert_eq!(monitor_named("move files to the second screen"), Some("2"));
        assert_eq!(monitor_named("notes on display two"), Some("2"));
        assert_eq!(monitor_named("put it on the other monitor"), Some("next"));
        assert_eq!(monitor_named("the 2nd monitor"), Some("2"));
        assert_eq!(monitor_named("take a screenshot"), None);
        assert_eq!(monitor_named("open notes"), None);
    }

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
        // Four columns since docs/one-bella-design.md; five are too many.
        assert_eq!(
            s("files notes network and the assistant").as_deref(),
            Some("Files and Notes and Network and Bella")
        );
        assert_eq!(s("two files two notes and network"), None);
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
        // Four columns since docs/one-bella-design.md; Bella once.
        assert_eq!(
            split_apps("Notes Files Network Bella").as_deref(),
            Some("Notes and Files and Network and Bella")
        );
        assert_eq!(
            split_apps("bella and bella and files").as_deref(),
            Some("Bella and Files")
        );
        assert_eq!(split_apps("Notes Files Network Files Notes"), None);
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
