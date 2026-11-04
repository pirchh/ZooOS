//! Bella: the assistant's window (docs/m5c2-design.md; named the
//! Assistant before 0.77.0). The user types
//! either a command (`search …`, `list …`, `inspect …`, `create …`, `undo`),
//! which goes straight to the file broker, or a request in words, which
//! ElephantHandler (the model service) turns into one command first.
//!
//! `create` always shows OtterHandler's plan and waits for `y` before
//! anything changes. Backspace on an empty line cancels a running request.
//!
//! Handles: the window channel (first) and a home directory handle (second),
//! which is moved into OtterHandler when it is started; Bella itself never
//! uses it. Both helpers start on first use.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use zoo_rt::app::{Key, Window, send_wait};
use zoo_rt::{
    Error, Handle, NO_HANDLE, channel_create, close, recv, send, sleep, spawn, spawn_with, try_recv,
};

zoo_rt::entry2!(main);

/// Window rows: status, input, then the transcript.
const ROWS: usize = 15;
const TRANSCRIPT: usize = ROWS - 2;
const MAX_INPUT: usize = 120;
use zoo_abi::bella::{
    asks_about_activity, asks_my_files, asks_what_bella_did, asks_what_remembered, fact_to_forget,
    fact_to_remember, file_to_summarise, follow_up, question_about_answer, small_talk,
    summary_stem, typed_command, wants_saved,
};
/// `args` without a trailing " on monitor N" or " on the next monitor",
/// and the monitor ("2", "next"; docs/monitors-design.md).
fn take_monitor(args: &str) -> (String, Option<String>) {
    let lower = args.to_ascii_lowercase();
    for (phrase, which) in [
        (" on the next monitor", Some("next")),
        (" on monitor ", None),
    ] {
        if let Some(at) = lower.find(phrase) {
            let m = match which {
                Some(w) => String::from(w),
                None => String::from(args[at + phrase.len()..].trim()),
            };
            if m == "next" || m.parse::<u8>().is_ok_and(|n| (1..=4).contains(&n)) {
                return (String::from(args[..at].trim()), Some(m));
            }
        }
    }
    (String::from(args.trim()), None)
}

/// Commands the memory may hold and replay (B4): never `undo`, and never
/// anything that is not a command (`remember`, `quit`, ...). `create`
/// still asks first.
const RECALLABLE: [&str; 15] = [
    "search", "list", "inspect", "create", "read", "open", "move", "trash", "launch", "info",
    "maximize", "minimize", "close", "split", "show",
];

fn recallable(command: &str) -> bool {
    RECALLABLE.contains(&command.split(' ').next().unwrap_or(""))
}
/// The model, in the initial filesystem (as ElephantHandler finds it).
const MODEL_FILE: &str = zoo_abi::CHAT_MODEL_FILE;
const HINT: &str = "Ask in words, or type a command (search, read, open, move, trash, info...).";
/// A remembered request at least this alike (percent of shared words)
/// is answered from memory, without the model (docs/a2-design.md).
const RECALL_AT: usize = 75;

enum Mode {
    Typing,
    /// Waiting for the model; the original request is kept for the log.
    Thinking,
    /// A command that changes things is shown (`create`'s plan, or what
    /// `trash` would throw away); `y` runs it (docs/access-design.md).
    Confirm(String),
}

struct App {
    win: Window,
    status: String,
    input: String,
    lines: VecDeque<String>,
    mode: Mode,
    /// Home handle until OtterHandler takes it.
    home: Handle,
    otter: Option<Handle>,
    elephant: Option<Handle>,
    /// The request in words being answered (by memory or the model).
    request: Option<String>,
    /// A request in words that was not answered well (the model was not
    /// there, failed, or its choice was refused): the next command that
    /// works is remembered as its answer (docs/a2-design.md).
    unanswered: Option<String>,
    /// When Enter was pressed on the request being answered, in
    /// microseconds since boot (docs/stage1-design.md).
    asked_at: u64,
    /// The model service ended: start it again (and warm it up) at this
    /// tick (docs/stage2-design.md §4).
    model_restart_at: Option<u64>,
    /// Failures of each helper since its last success, for the back-off.
    model_failures: u32,
    broker_failures: u32,
    /// No new broker before this tick.
    broker_retry_at: u64,
    /// The broker's result lines for the last command (for the
    /// conversation; docs/talk-design.md §6).
    last_output: Vec<String>,
    /// The last exchange in a sentence, given with a follow-up.
    last_exchange: Option<String>,
    /// The line being answered in words (for `last_exchange`).
    talking: Option<String>,
    /// The request being chosen for is a follow-up, sent with context.
    following: bool,
    /// The steps of a request of several still to run, and the last one
    /// run, for "it" (docs/multi-step-design.md).
    steps: VecDeque<String>,
    last_step: String,
    /// A question about the screen, waiting for the window server's
    /// `context:` answer (docs/context-design.md).
    asking_context: Option<String>,
    /// While set, the broker's `| ` lines are collected here instead of
    /// shown (a file being read for a summary).
    capture: Option<String>,
    /// The request whose summary the model is writing
    /// (docs/summary-design.md).
    summarising: Option<String>,
    /// The file being summarised, and then (file, summary) once shown:
    /// "save that" writes it beside the file (docs/save-design.md).
    summary_of: Option<String>,
    last_summary: Option<(String, String)>,
    /// The transcript's last line is a reply still being written
    /// (docs/stream-design.md).
    streaming: bool,
    /// Which acknowledgement line comes next (docs/stream-design.md).
    acks: usize,
    /// The files an answer from files came from, shown under it.
    sources: Option<String>,
    /// MagpieHandler, the index of the user's files by meaning
    /// (docs/vectors-design.md), and whether its first index is built.
    magpie: Option<Handle>,
    magpie_ready: bool,
}

/// Most bytes of an exchange kept for the conversation (the engine's
/// `assist::MAX_CONTEXT`).
const EXCHANGE_BYTES: usize = 300;

/// First wait before restarting a helper that ended (0.5 s); it doubles
/// with each failure in a row, up to `MAX_BACKOFF`.
const BACKOFF: u64 = 50;
const MAX_BACKOFF: u64 = 1600;
/// Longest wait for the broker's answer (10 s, docs/stage2-design.md §4).
const BROKER_DEADLINE: u64 = 1000;

/// `s` cut to `EXCHANGE_BYTES` at a character boundary, without tabs or
/// line breaks (they separate fields in messages).
fn shorten(s: String) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if out.len() + c.len_utf8() > EXCHANGE_BYTES {
            break;
        }
        out.push(if c == '\t' || c == '\n' { ' ' } else { c });
    }
    out
}

/// Said at once when a request will take the model a while
/// (docs/stream-design.md); taken in turn so they do not repeat.
const READING: [&str; 3] = [
    "Let me read that file, one moment...",
    "Reading it now, hang tight...",
    "On it, reading the file...",
];
const SEARCHING: [&str; 3] = [
    "Let me check your files...",
    "Looking through your files, one sec...",
    "Checking what you wrote...",
];
const LOOKING: [&str; 3] = [
    "Let me look at your screen...",
    "One sec, checking what's open...",
    "Looking at your windows...",
];

/// How long a question to the file index may take (ticks): the index is
/// brought up to date first, which embeds any changed files.
const INDEX_DEADLINE: u64 = 3000;

fn backoff(failures: u32) -> u64 {
    (BACKOFF << failures.saturating_sub(1).min(5)).min(MAX_BACKOFF)
}

fn main(chan: u64, home: u64) -> u8 {
    let mut p = App {
        win: Window::new(chan),
        status: String::from(HINT),
        input: String::new(),
        lines: VecDeque::new(),
        mode: Mode::Typing,
        home,
        otter: None,
        elephant: None,
        request: None,
        unanswered: None,
        asked_at: 0,
        model_restart_at: None,
        model_failures: 0,
        broker_failures: 0,
        broker_retry_at: 0,
        last_output: Vec::new(),
        last_exchange: None,
        talking: None,
        following: false,
        steps: VecDeque::new(),
        last_step: String::new(),
        asking_context: None,
        capture: None,
        summarising: None,
        summary_of: None,
        last_summary: None,
        streaming: false,
        acks: 0,
        sources: None,
        magpie: None,
        magpie_ready: false,
    };
    if p.win.title("Bella").is_err() || p.win.lines(ROWS).is_err() || p.draw().is_err() {
        return 0;
    }
    p.warm();
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        let mut busy = false;
        // Take every queued key before redrawing, so fast typing is not
        // dropped by a full queue in the window server.
        loop {
            match try_recv(chan, &mut buf) {
                Ok(n) => {
                    busy = true;
                    if let Some(ctx) = buf[..n].strip_prefix(b"context:") {
                        let ctx = String::from(core::str::from_utf8(ctx).unwrap_or(""));
                        p.got_context(&ctx);
                    } else if let Some(said) = buf[..n].strip_prefix(b"window-done:") {
                        let said = String::from(core::str::from_utf8(said).unwrap_or(""));
                        p.window_done(&said);
                    } else if let Some(key) = Key::parse(&buf[..n]) {
                        p.key(key);
                    }
                }
                Err(Error::Again) => break,
                Err(_) => return 0,
            }
        }
        if let Some(e) = p.elephant {
            // Replies matter while thinking; otherwise only the channel
            // closing (the service ended) is looked for.
            let thinking = matches!(p.mode, Mode::Thinking);
            match try_recv(e, &mut buf) {
                Ok(n) if thinking => {
                    busy = true;
                    let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                    p.model_reply(&msg);
                }
                Ok(_) | Err(Error::Again) => {}
                Err(_) => {
                    busy = true;
                    p.model_ended(thinking);
                }
            }
        }
        // The file index says when its first index is built.
        if let Some(m) = p.magpie
            && !p.magpie_ready
        {
            match try_recv(m, &mut buf) {
                Ok(n) => {
                    let msg = core::str::from_utf8(&buf[..n]).unwrap_or("");
                    if let Some(lines) = msg.strip_prefix("ready ") {
                        p.magpie_ready = true;
                        zoo_rt::log!("Bella: the file index is ready ({} lines)", lines);
                    } else {
                        zoo_rt::log!("Bella: the file index: {}", msg);
                        let _ = close(m);
                        p.magpie = None;
                    }
                }
                Err(Error::Again) => {}
                Err(_) => {
                    let _ = close(m);
                    p.magpie = None;
                }
            }
        }
        // The next step of a request of several, once the last is done
        // (docs/multi-step-design.md).
        if matches!(p.mode, Mode::Typing) && !p.steps.is_empty() {
            busy = true;
            p.next_step();
        }
        if p.model_restart_at.is_some_and(|at| zoo_rt::ticks() >= at) {
            p.model_restart_at = None;
            zoo_rt::log!("Bella: starting the model service again");
            p.warm();
        }
        if busy {
            if p.draw().is_err() {
                return 0;
            }
        } else {
            sleep(2);
        }
    }
}

impl App {
    fn say(&mut self, text: &str) {
        if self.lines.len() == TRANSCRIPT {
            self.lines.pop_front();
        }
        self.lines.push_back(String::from(text));
    }

    fn draw(&self) -> Result<(), Error> {
        self.win.line(0, &self.status)?;
        let mut input = String::from("> ");
        input.push_str(&self.input);
        if matches!(self.mode, Mode::Typing) {
            input.push('_');
        }
        self.win.line(1, &input)?;
        for i in 0..TRANSCRIPT {
            self.win
                .line(2 + i, self.lines.get(i).map_or("", String::as_str))?;
        }
        self.win.select(None)?;
        self.win.show()
    }

    fn key(&mut self, key: Key) {
        match (&self.mode, key) {
            (Mode::Thinking, Key::Back) => {
                self.drop_steps();
                if let Some(e) = self.elephant {
                    let _ = send(e, b"cancel");
                    zoo_rt::log!("Bella: cancel sent at tick {}", zoo_rt::ticks());
                }
                self.status = String::from("Cancelling...");
            }
            (Mode::Thinking, _) => {}
            (Mode::Confirm(req), Key::Char('y')) => {
                let req = req.clone();
                self.mode = Mode::Typing;
                self.status = String::from(HINT);
                if self.broker(&req) {
                    self.learn(&req);
                }
            }
            (Mode::Confirm(req), Key::Char('n') | Key::Back) => {
                let what = if req.starts_with("create ") {
                    "Not created."
                } else if req.starts_with("fact-forget ") {
                    "Kept."
                } else {
                    "Left where it is."
                };
                self.drop_steps();
                self.mode = Mode::Typing;
                self.status = String::from(HINT);
                self.say(what);
                // The choice was wrong: a command typed next answers it.
                if let Some(r) = self.request.take() {
                    self.unanswered = Some(r);
                }
            }
            (Mode::Confirm(_), _) => {}
            (Mode::Typing, Key::Char(c)) if self.input.len() < MAX_INPUT => self.input.push(c),
            (Mode::Typing, Key::Back) => {
                self.input.pop();
            }
            (Mode::Typing, Key::Enter) => {
                let text = core::mem::take(&mut self.input);
                let text = text.trim();
                if !text.is_empty() {
                    let mut echo = String::from("You: ");
                    echo.push_str(text);
                    self.say(&echo);
                    self.submit(&String::from(text));
                }
            }
            _ => {}
        }
    }

    fn submit(&mut self, text: &str) {
        // "Hey Bella, ..." is how a spoken request starts: the rest is the
        // request ("hey" alone made it small talk).
        let text = zoo_abi::bella::strip_address(text);
        // Several steps: each runs on its own, in order
        // (docs/multi-step-design.md).
        // A single typed command never has two steps ("move a to b").
        let steps = zoo_infer::steps::split_steps(text);
        if !steps.is_empty() {
            zoo_rt::log!("Bella: {} steps: {}", steps.len(), steps.join(" | "));
            self.steps = steps.into();
            self.last_step.clear();
            self.next_step();
            return;
        }
        self.last_step.clear();
        self.submit_one(text);
    }

    /// Runs the next step, with "it" naming the apps of the one before.
    fn next_step(&mut self) {
        let Some(step) = self.steps.pop_front() else {
            return;
        };
        let step = zoo_infer::assist::resolve_it(&step, &self.last_step);
        zoo_rt::log!("Bella: step: {}", step);
        self.last_step = step.clone();
        self.submit_one(&step);
    }

    fn submit_one(&mut self, text: &str) {
        self.asked_at = zoo_rt::now_us();
        if typed_command(text) {
            // Typed by the user: it may answer an earlier request.
            self.request = self.unanswered.take();
            self.command(text);
            self.answered("a typed command");
            self.exchanged(text, text);
        } else if wants_saved(text) && self.last_summary.is_some() {
            self.request = None;
            self.save_summary();
        } else if asks_my_files(text) {
            self.request = None;
            self.answer_from_files(text);
        } else if let Some(path) = file_to_summarise(text) {
            self.request = None;
            self.summarise(text, path);
        } else if asks_about_activity(text) {
            // What is on the screen: asked of the window server, answered
            // in words (docs/context-design.md).
            self.request = None;
            self.ack(&LOOKING);
            if self.win.context().is_ok() {
                self.asking_context = Some(String::from(text));
                self.mode = Mode::Thinking;
                self.status = String::from("Looking at the screen...");
            }
        } else if let Some(fact) = fact_to_remember(text) {
            // Facts the user tells Bella (docs/facts-design.md).
            let mut req = String::from("fact-add ");
            req.push_str(fact);
            if self.broker(&req) {
                self.say("Bella: I'll remember that.");
                zoo_rt::log!("Bella: remembered a fact");
            }
            self.request = None;
        } else if asks_what_bella_did(text) {
            self.say("Bella: What I changed, newest last:");
            if !self.broker("audit-list") {
                self.say("  nothing yet.");
            }
            self.request = None;
        } else if asks_what_remembered(text) {
            self.say("Bella: You told me:");
            if !self.broker("fact-list") {
                self.say("  nothing yet.");
            }
            self.request = None;
        } else if let Some(about) = fact_to_forget(text) {
            // Forgetting is deleting: ask first (docs/access-design.md).
            let mut q = String::from("Forget what you told me about ");
            q.push_str(about);
            q.push('?');
            self.say(&q);
            self.status = String::from("Press y to forget it, n to keep it.");
            self.mode = Mode::Confirm(alloc::format!("fact-forget {}", about));
            self.request = None;
        } else if question_about_answer(text) && self.last_exchange.is_some() {
            // "is that it?": answered in words from the conversation
            // (docs/talk-design.md §6).
            zoo_rt::log!("Bella: a question about the last answer");
            self.request = None;
            self.ask_talk(text);
        } else if small_talk(text) {
            self.request = None;
            self.ask_talk(text);
        } else if !self.facts_about(text, 2).is_empty() {
            // Something the user told Bella answers it ("when is my
            // dentist?"): a reply in words with those facts.
            self.request = None;
            self.ask_talk(text);
        } else {
            self.request = Some(String::from(text));
            if self.recall(text) {
                self.answered("memory");
            } else {
                self.ask_model(text);
            }
        }
    }

    /// A command answered `asked`: keep the exchange for follow-ups, and
    /// note it in the model's conversation so later replies know it
    /// (docs/talk-design.md §6). File names in the results reach the model
    /// this way; its replies are only shown, and destructive commands ask.
    fn exchanged(&mut self, asked: &str, cmd: &str) {
        let mut said = String::from("I ran `");
        said.push_str(cmd);
        said.push('`');
        if self.last_output.is_empty() {
            said.push_str(" and it showed nothing.");
        } else {
            said.push_str(" and it showed: ");
            for (k, l) in self.last_output.iter().enumerate() {
                if k > 0 {
                    said.push_str("; ");
                }
                said.push_str(l.trim());
            }
            said.push('.');
        }
        self.remember_exchange(asked, &said);
    }

    /// Keeps an exchange that was not a reply in words: as the last
    /// exchange for a follow-up, and in the model's conversation.
    fn remember_exchange(&mut self, asked: &str, said: &str) {
        let said = shorten(String::from(said));
        let mut e = String::from("The user asked: ");
        e.push_str(asked);
        e.push_str(". Bella: ");
        e.push_str(&said);
        self.last_exchange = Some(shorten(e));
        // Between the steps of a request of several, the model's next
        // choice should not wait for it to read a note
        // (docs/multi-step-design.md).
        if !self.steps.is_empty() {
            return;
        }
        if let Some(el) = self.elephant {
            let mut msg = String::from("note ");
            msg.push_str(asked);
            msg.push('\t');
            msg.push_str(&said);
            let _ = send_wait(el, msg.as_bytes());
        }
    }

    /// The timing line for one request: from Enter to its answer shown,
    /// through the broker (docs/stage1-design.md).
    fn answered(&self, by: &str) {
        zoo_rt::log!(
            "Bella: answered in {} ms by {}",
            (zoo_rt::now_us() - self.asked_at) / 1000,
            by
        );
    }

    /// The rest of a request of several is not run (cancelled, or a
    /// confirmation refused).
    fn drop_steps(&mut self) {
        if !self.steps.is_empty() {
            zoo_rt::log!("Bella: {} steps not run", self.steps.len());
            self.steps.clear();
        }
    }

    /// A command, typed or chosen. `create` asks first; others that work
    /// teach the memory the request they answer.
    fn command(&mut self, text: &str) {
        if let Some(path) = text.strip_prefix("create ") {
            let path = String::from(path.trim());
            let mut plan = String::from("plan ");
            plan.push_str(&path);
            if self.broker(&plan) {
                self.status = String::from("Press y to create, n to leave it.");
                self.mode = Mode::Confirm(alloc::format!("create {}", path));
            }
        } else if let Some(path) = text.strip_prefix("trash ") {
            // The owner's rule: Bella asks before anything goes away
            // (docs/access-design.md). The Trash keeps it, for Put Back.
            let path = String::from(path.trim());
            let mut look = String::from("inspect ");
            look.push_str(&path);
            if self.broker(&look) {
                // The broker's spelling: it resolved letter case and names.
                let real = self
                    .last_output
                    .first()
                    .and_then(|l| l.split_once(": "))
                    .map_or(path.clone(), |(p, _)| String::from(p));
                let mut q = String::from("Move ");
                q.push_str(&real);
                q.push_str(" to the Trash?");
                self.say(&q);
                self.status = String::from("Press y to move it to the Trash, n to leave it.");
                self.mode = Mode::Confirm(alloc::format!("trash {}", real));
            }
        } else if let Some(path) = text.strip_prefix("open ") {
            self.open(path.trim());
        } else if text.starts_with("write ") {
            // A line in Notes (docs/actions-design.md): the whole command,
            // so "write down X" loses its "down".
            let t = zoo_infer::assist::note_text(text);
            if t.is_empty() || self.win.act("notes", "write", &t).is_err() {
                self.say("Error: nothing to write.");
            } else {
                zoo_rt::log!("Bella: asked Notes to write a line");
            }
        } else if let Some(a) = text.strip_prefix("browse ") {
            // A page in Network (docs/actions-design.md).
            let a = zoo_infer::assist::address_in(a).unwrap_or(a.trim());
            if self.win.act("network", "fetch", a).is_ok() {
                zoo_rt::log!("Bella: asked Network to fetch {}", a);
            }
        } else if let Some(apps) = text.strip_prefix("launch ") {
            // "launch Notes and Files": each (docs/multi-step-design.md).
            for app in String::from(apps).split(" and ") {
                self.launch(app.trim());
            }
        } else if let Some(apps) = text
            .strip_prefix("split ")
            .or((text == "split").then_some(""))
        {
            let (apps, monitor) = take_monitor(apps);
            self.split(apps.trim(), monitor.as_deref());
        } else if let Some((action, app)) = ["maximize", "minimize", "close", "show"]
            .iter()
            .find_map(|a| Some((*a, text.strip_prefix(a)?.strip_prefix(' ')?)))
        {
            // "... on monitor 2" (docs/monitors-design.md).
            let (apps, monitor) = take_monitor(app);
            // `show`: the window moves to that monitor.
            let action = if action == "show" { "move" } else { action };
            let action = match &monitor {
                Some(m) => alloc::format!("{action}@{m}"),
                None => String::from(action),
            };
            for app in apps.split(" and ") {
                self.window_action(&action, app.trim());
            }
        } else if self.broker(text) {
            self.learn(text);
        } else {
            if let Some(r) = self.request.take() {
                self.unanswered = Some(r);
            }
        }
    }

    /// Reads `path` through the broker and has the model summarise it in
    /// a call of its own (docs/summary-design.md): the text is data, it is
    /// capped, and it never joins the conversation.
    fn summarise(&mut self, request: &str, path: &str) {
        self.ack(&READING);
        let mut req = String::from("read ");
        req.push_str(path);
        self.capture = Some(String::new());
        let ok = self.broker(&req);
        let text = self.capture.take().unwrap_or_default();
        if !ok {
            return;
        }
        let mut cut = String::new();
        for c in text.chars() {
            if cut.len() + c.len_utf8() > zoo_infer::talk::MAX_SUMMARY_TEXT {
                break;
            }
            cut.push(if c == '\t' { ' ' } else { c });
        }
        if cut.trim().is_empty() {
            self.say("Bella: That file is empty.");
            return;
        }
        zoo_rt::log!("Bella: summarising {} ({} bytes)", path, cut.len());
        if !self.start_model() {
            self.say("Error: cannot start the model service.");
            return;
        }
        let mut msg = String::from("summarise ");
        msg.push_str(path);
        msg.push('\t');
        msg.push_str(&cut);
        let Some(e) = self.elephant else { return };
        if send_wait(e, msg.as_bytes()).is_err() {
            self.model_ended(false);
            return;
        }
        self.summarising = Some(String::from(request));
        self.summary_of = Some(String::from(path));
        self.mode = Mode::Thinking;
        self.status = String::from("Reading... (Backspace cancels)");
    }

    /// Writes the last summary beside its file (docs/save-design.md); the
    /// broker refuses to replace a file that exists.
    fn save_summary(&mut self) {
        let Some((file, summary)) = self.last_summary.clone() else {
            return;
        };
        let mut to = String::from(summary_stem(&file));
        to.push_str("-summary.txt");
        let mut req = String::from("write ");
        req.push_str(&to);
        req.push('\t');
        req.push_str("Summary of ");
        req.push_str(&file);
        req.push_str(":\n");
        req.push_str(&summary);
        req.push('\n');
        if self.broker(&req) {
            self.last_summary = None;
            let mut l = String::from("Bella: Saved. Open it with: open ");
            l.push_str(&to);
            self.say(&l);
        }
    }

    /// Finds lines of the user's files that share words with `question`
    /// and has the model answer from them in a call of its own
    /// (docs/lookup-design.md).
    fn answer_from_files(&mut self, question: &str) {
        self.ack(&SEARCHING);
        // By meaning, from the index; by words when there is none.
        let found = match self.index_lines(question) {
            Some(lines) => lines,
            None => {
                let mut req = String::from("lookup ");
                req.push_str(question);
                self.capture = Some(String::new());
                let _ = self.broker(&req);
                self.capture.take().unwrap_or_default()
            }
        };
        if found.trim().is_empty() {
            self.say("Bella: I couldn't find anything about that in your files.");
            zoo_rt::log!("Bella: nothing in the files for that");
            return;
        }
        let mut cut = String::new();
        for c in found.chars() {
            if cut.len() + c.len_utf8() > zoo_infer::talk::MAX_SUMMARY_TEXT {
                break;
            }
            cut.push(if c == '\t' { ' ' } else { c });
        }
        // Where the answer comes from, shown under it.
        let mut from = String::new();
        for line in cut.lines() {
            if let Some((path, _)) = line.split_once(": ")
                && !from.split(", ").any(|p| p == path)
            {
                if !from.is_empty() {
                    from.push_str(", ");
                }
                from.push_str(path);
            }
        }
        zoo_rt::log!("Bella: answering from {}", from);
        if !self.start_model() {
            self.say("Error: cannot start the model service.");
            return;
        }
        let mut msg = String::from("answer ");
        msg.push_str(question);
        msg.push('\t');
        msg.push_str(&cut);
        let Some(e) = self.elephant else { return };
        if send_wait(e, msg.as_bytes()).is_err() {
            self.model_ended(false);
            return;
        }
        self.summarising = Some(String::from(question));
        self.sources = Some(from);
        self.mode = Mode::Thinking;
        self.status = String::from("Reading your files... (Backspace cancels)");
    }

    /// Says the next of `lines` at once, before a slow answer.
    fn ack(&mut self, lines: &[&str]) {
        let line = lines[self.acks % lines.len()];
        self.acks += 1;
        let mut l = String::from("Bella: ");
        l.push_str(line);
        self.say(&l);
        zoo_rt::log!("Bella: said first: {}", line);
    }

    /// The window server's answer to `context?`: the question waiting for
    /// it is answered in words with what is on the screen.
    fn got_context(&mut self, ctx: &str) {
        let Some(question) = self.asking_context.take() else {
            return;
        };
        zoo_rt::log!("Bella: the screen: {}", ctx);
        self.mode = Mode::Typing;
        self.status = String::from(HINT);
        // Whose screen: the model otherwise answers as if it were working.
        let mut seen = String::from("On my screen: ");
        seen.push_str(ctx);
        self.talk_with(&question, &seen);
    }

    /// The facts sharing at least `min` words with `text`, joined with
    /// "; " (at most `EXCHANGE_BYTES`), without showing them.
    fn facts_about(&mut self, text: &str, min: usize) -> String {
        let Some(o) = self.otter() else {
            return String::new();
        };
        let mut req = String::from("fact-find ");
        req.push_str(text);
        if send_wait(o, req.as_bytes()).is_err() {
            return String::new();
        }
        let mut out = String::new();
        let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
        loop {
            let Ok(n) = zoo_rt::recv(o, &mut buf) else {
                self.otter = None;
                return String::new();
            };
            let line = core::str::from_utf8(&buf[..n]).unwrap_or("");
            let Some(rest) = line.strip_prefix("| ") else {
                break;
            };
            let (score, fact) = rest.split_once(' ').unwrap_or(("0", rest));
            if score.parse::<usize>().unwrap_or(0) >= min {
                if !out.is_empty() {
                    out.push_str("; ");
                }
                out.push_str(fact);
            }
        }
        shorten(out)
    }

    /// `open <path>`: a folder in Files, a file in Notes, through the
    /// window server (docs/access-design.md).
    fn open(&mut self, path: &str) {
        let mut look = String::from("inspect ");
        look.push_str(path);
        if !self.broker(&look) {
            return;
        }
        let folder = self
            .last_output
            .first()
            .is_some_and(|l| l.contains(": folder,"));
        // The broker resolved the name (letter case, `.`): use its spelling.
        let shown = self
            .last_output
            .first()
            .and_then(|l| l.split_once(": "))
            .map_or(String::from(path), |(p, _)| String::from(p));
        let real = if shown == "home" {
            String::new()
        } else {
            shown
        };
        let program = if folder { "files" } else { "notes" };
        if self.win.open(program, &real).is_ok() {
            let mut l = String::from("Opened ");
            l.push_str(if real.is_empty() {
                "your home folder"
            } else {
                &real
            });
            l.push_str(if folder { " in Files." } else { " in Notes." });
            self.say(&l);
            zoo_rt::log!("Bella: opened `{}` in {}", real, program);
            self.learn_open(path);
        }
    }

    /// The program of the application called `app`, or says there is
    /// none.
    fn program_of(&mut self, app: &str) -> Option<&'static str> {
        let a = app.to_ascii_lowercase();
        let a = a.strip_prefix("the ").unwrap_or(&a);
        Some(match a.trim_end_matches(" app") {
            "files" | "finder" | "file browser" => "files",
            "notes" | "editor" | "text editor" => "notes",
            "network" | "internet" | "web" => "network",
            "crash test" | "crash-test" => "crash-test",
            _ => {
                let mut l = String::from("Error: no app called ");
                l.push_str(app);
                self.say(&l);
                return None;
            }
        })
    }

    /// `split <apps>`: up to three apps side by side, left to right; no app
    /// named: the two front windows (docs/tiling-design.md). Bella may be
    /// named here ("Bella", "the assistant", "you"), unlike in the other
    /// window commands.
    fn split(&mut self, apps: &str, monitor: Option<&str>) {
        let Some(names) = zoo_infer::assist::split_apps(apps) else {
            self.say("Error: split takes up to three apps, e.g. split Notes and Bella");
            return;
        };
        if names.is_empty() {
            // "split these two windows": the window server picks them.
            if self.win.split_front().is_ok() {
                zoo_rt::log!("Bella: asked to split the front windows");
                self.learn("split");
            }
            return;
        }
        let names: Vec<&str> = names.split(" and ").collect();
        let n = names.len();
        for (k, name) in names.iter().enumerate() {
            let program = match *name {
                "Bella" => "bella",
                "Files" => "files",
                "Notes" => "notes",
                _ => "network",
            };
            let action = match monitor {
                Some(m) => alloc::format!("col{k}of{n}@{m}"),
                None => alloc::format!("col{k}of{n}"),
            };
            // The same app again: its next window (docs/tiling-design.md).
            let m = names[..k].iter().filter(|x| *x == name).count();
            let target = if m == 0 {
                String::from(program)
            } else {
                alloc::format!("{program}#{m}")
            };
            if self.win.window(&action, &target).is_err() {
                return;
            }
            zoo_rt::log!(
                "Bella: asked to put {} in column {} of {}",
                program,
                k + 1,
                n
            );
        }
        let mut cmd = String::from("split ");
        cmd.push_str(&names.join(" and "));
        self.learn(&cmd);
    }

    /// `maximize`, `minimize` or `close <app>`: asks the window server,
    /// which answers with what it did (`window_done`;
    /// docs/window-actions-design.md).
    fn window_action(&mut self, action: &str, app: &str) {
        // Bella's own window may be placed, never closed or hidden this
        // way (docs/monitors-design.md).
        let me = matches!(
            app.to_ascii_lowercase().as_str(),
            "bella" | "assistant" | "the assistant" | "you" | "yourself"
        );
        let program = if me && !action.starts_with("close") && !action.starts_with("minimize") {
            "bella"
        } else {
            match self.program_of(app) {
                Some(p) => p,
                None => return,
            }
        };
        if self.win.window(action, program).is_ok() {
            zoo_rt::log!("Bella: asked to {} {}", action, program);
            let mut cmd = String::from(action);
            cmd.push(' ');
            cmd.push_str(app);
            self.learn(&cmd);
        }
    }

    /// The window server's sentence for a window action.
    fn window_done(&mut self, said: &str) {
        zoo_rt::log!("Bella: the window server says: {}", said);
        self.say(said);
        let _ = self.draw();
    }

    /// `launch <app>`: opens an application by its name.
    fn launch(&mut self, app: &str) {
        let Some(program) = self.program_of(app) else {
            return;
        };
        if self.win.launch(program).is_ok() {
            let mut l = String::from("Opened ");
            l.push_str(app);
            l.push('.');
            self.say(&l);
            zoo_rt::log!("Bella: launched {}", program);
            let mut cmd = String::from("launch ");
            cmd.push_str(app);
            self.learn(&cmd);
        }
    }

    /// `open` worked: remember it like a command.
    fn learn_open(&mut self, path: &str) {
        let mut cmd = String::from("open ");
        cmd.push_str(path);
        self.learn(&cmd);
    }

    /// `command` answered the current request: remember the pair.
    fn learn(&mut self, command: &str) {
        let Some(request) = self.request.take() else {
            return;
        };
        if !recallable(command) {
            return;
        }
        let msg = alloc::format!("remember {}\t{}", request, command);
        let quiet = self.lines.len();
        if self.broker(&msg) {
            zoo_rt::log!("Bella: learned `{}` -> `{}`", request, command);
        }
        // `remember` prints nothing; keep the transcript as it was.
        self.lines.truncate(quiet);
    }

    /// Answers `text` from memory if a remembered request is alike enough;
    /// returns whether it did.
    fn recall(&mut self, text: &str) -> bool {
        let Some(o) = self.otter() else {
            return false;
        };
        let msg = alloc::format!("recall {}", text);
        if send_wait(o, msg.as_bytes()).is_err() {
            return false;
        }
        let mut best: Option<(usize, String)> = None;
        let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
        loop {
            let Ok(n) = recv(o, &mut buf) else {
                self.otter = None;
                return false;
            };
            let line = core::str::from_utf8(&buf[..n]).unwrap_or("");
            if let Some(rest) = line.strip_prefix("| ") {
                let (score, cmd) = rest.split_once(' ').unwrap_or((rest, ""));
                best = score.parse().ok().map(|s| (s, String::from(cmd)));
            } else {
                break;
            }
        }
        let Some((score, cmd)) = best.filter(|(s, _)| *s >= RECALL_AT) else {
            return false;
        };
        // The memory file is in home, which other programs can write:
        // only replay what the memory may hold (B4).
        if !recallable(&cmd) || cmd.contains(['\t', '\n']) {
            zoo_rt::log!("Bella: ignored remembered `{}`", cmd);
            return false;
        }
        zoo_rt::log!("Bella: recalled `{}` ({}% alike)", cmd, score);
        let mut l = String::from("Bella (remembered): ");
        l.push_str(&cmd);
        self.say(&l);
        // Not learned again; a command typed next corrects it (B3).
        let request = self.request.take();
        self.command(&cmd);
        self.exchanged(text, &cmd);
        self.unanswered = request;
        true
    }

    /// The model service ended (crashed or killed): say so if a request
    /// was waiting, and start it again after a back-off
    /// (docs/stage2-design.md §4).
    fn model_ended(&mut self, thinking: bool) {
        if let Some(e) = self.elephant.take() {
            let _ = close(e);
        }
        self.model_failures += 1;
        let wait = backoff(self.model_failures);
        zoo_rt::log!(
            "Bella: the model service ended; starting it again in {} ms",
            wait * 10
        );
        self.model_restart_at = Some(zoo_rt::ticks() + wait);
        // A reply cut off mid-way stays as written; the next starts a line.
        self.streaming = false;
        if thinking {
            self.mode = Mode::Typing;
            self.status = String::from(HINT);
            self.say("Error: the model service stopped; it is starting again.");
            if let Some(r) = self.request.take() {
                self.unanswered = Some(r);
            }
        }
    }

    /// Starts OtterHandler on first use with a copy of the home handle, so
    /// it can be started again if it ends (docs/stage2-design.md §4).
    fn otter(&mut self) -> Option<Handle> {
        if self.otter.is_none() && self.home != NO_HANDLE {
            if zoo_rt::ticks() < self.broker_retry_at {
                return None;
            }
            let (mine, theirs) = channel_create().ok()?;
            let Ok(home) = zoo_rt::dir_open(self.home, "", zoo_rt::dir_rights::ALL) else {
                let _ = close(mine);
                let _ = close(theirs);
                return None;
            };
            match spawn_with("otter-handler", [Some(theirs), Some(home)]) {
                Ok(process) => {
                    // The process handle is not needed: the channel closing
                    // tells us if it ends.
                    let _ = close(process);
                    self.otter = Some(mine);
                }
                Err(_) => {
                    let _ = close(mine);
                }
            }
        }
        self.otter
    }

    /// Sends one request to the broker and shows its reply. Returns true on
    /// `ok`.
    fn broker(&mut self, req: &str) -> bool {
        let Some(o) = self.otter() else {
            self.say("Error: the file broker is not available.");
            return false;
        };
        if send_wait(o, req.as_bytes()).is_err() {
            self.broker_ended("stopped");
            return false;
        }
        if !req.starts_with("remember ") && !req.starts_with("recall ") {
            self.last_output.clear();
        }
        let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
        let deadline = zoo_rt::ticks() + BROKER_DEADLINE;
        loop {
            // Bounded (B14): a broker that does not answer in 10 s is
            // ended and started again later.
            let left = deadline.saturating_sub(zoo_rt::ticks());
            if left == 0 || zoo_rt::wait_any(&[o], left).is_err() {
                self.broker_ended("did not answer in 10 s");
                return false;
            }
            let n = match zoo_rt::try_recv(o, &mut buf) {
                Ok(n) => n,
                Err(Error::Again) => continue,
                Err(_) => {
                    self.broker_ended("stopped");
                    return false;
                }
            };
            self.broker_failures = 0;
            let line = core::str::from_utf8(&buf[..n]).unwrap_or("?");
            if let Some(rest) = line.strip_prefix("| ")
                && let Some(c) = self.capture.as_mut()
            {
                // `read`'s note of what it left out is not file text.
                if !(rest.starts_with('(') && rest.ends_with(" more bytes)")) {
                    c.push_str(rest);
                    c.push('\n');
                }
            } else if let Some(rest) = line.strip_prefix("| ") {
                let mut l = String::from("  ");
                l.push_str(rest);
                self.say(&l);
                if self.last_output.len() < 8 {
                    self.last_output.push(String::from(rest));
                }
            } else if line == "ok" {
                return true;
            } else if line == "none" {
                // It ran but found nothing: not an answer to learn (B3).
                return false;
            } else {
                let mut l = String::from("Error: ");
                l.push_str(line.strip_prefix("err ").unwrap_or(line));
                self.say(&l);
                return false;
            }
        }
    }

    /// The broker ended or stopped answering: drop it; the next request
    /// starts a new one after a back-off (docs/stage2-design.md §4).
    fn broker_ended(&mut self, why: &str) {
        if let Some(o) = self.otter.take() {
            let _ = close(o);
        }
        self.broker_failures += 1;
        let wait = backoff(self.broker_failures);
        self.broker_retry_at = zoo_rt::ticks() + wait;
        zoo_rt::log!(
            "Bella: the file broker {}; a new one in {} ms",
            why,
            wait * 10
        );
        let mut l = String::from("Error: the file broker ");
        l.push_str(why);
        l.push('.');
        self.say(&l);
    }

    /// Starts the model service and has it read the system prompt now, in
    /// the background, when the model is installed (docs/stage2-design.md).
    fn warm(&mut self) {
        self.start_magpie();
        let installed = zoo_rt::fs_find(MODEL_FILE).is_some() || zoo_rt::model_size().is_ok();
        if !installed || !self.start_model() {
            return;
        }
        if let Some(e) = self.elephant {
            let _ = send_wait(e, b"warm");
        }
    }

    /// Starts MagpieHandler, the file index (docs/vectors-design.md),
    /// when the model disk or the boot image holds an embedding model.
    fn start_magpie(&mut self) {
        if self.magpie.is_some()
            || self.home == NO_HANDLE
            || (zoo_rt::model_size_of(zoo_abi::MODEL_EMBED).is_err()
                && zoo_rt::fs_find(zoo_abi::EMBED_MODEL_FILE).is_none())
        {
            return;
        }
        let Ok((mine, theirs)) = channel_create() else {
            return;
        };
        let Ok(home) = zoo_rt::dir_open(self.home, "", zoo_rt::dir_rights::ALL) else {
            let _ = close(mine);
            let _ = close(theirs);
            return;
        };
        match spawn_with("magpie-handler", [Some(theirs), Some(home)]) {
            Ok(process) => {
                let _ = close(process);
                self.magpie = Some(mine);
                zoo_rt::log!("Bella: starting the file index");
            }
            Err(_) => {
                let _ = close(mine);
            }
        }
    }

    /// Lines of the user's files nearest `question` in meaning, from the
    /// index (`path: line` per line); `None` when the index cannot answer,
    /// so the word lookup is used instead.
    fn index_lines(&mut self, question: &str) -> Option<String> {
        let m = self.magpie.filter(|_| self.magpie_ready)?;
        let mut req = String::from("query ");
        req.push_str(question);
        if send_wait(m, req.as_bytes()).is_err() {
            self.magpie = None;
            return None;
        }
        let mut out = String::new();
        let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
        let deadline = zoo_rt::ticks() + INDEX_DEADLINE;
        loop {
            let left = deadline.saturating_sub(zoo_rt::ticks());
            if left == 0 || zoo_rt::wait_any(&[m], left).is_err() {
                zoo_rt::log!("Bella: the file index did not answer in time");
                let _ = close(m);
                self.magpie = None;
                return None;
            }
            let n = match try_recv(m, &mut buf) {
                Ok(n) => n,
                Err(Error::Again) => continue,
                Err(_) => {
                    self.magpie = None;
                    return None;
                }
            };
            let line = core::str::from_utf8(&buf[..n]).unwrap_or("");
            if let Some(rest) = line.strip_prefix("| ") {
                out.push_str(rest);
                out.push('\n');
            } else if line == "ok" || line == "none" {
                zoo_rt::log!("Bella: {} lines from the file index", out.lines().count());
                return Some(out);
            } else {
                return None;
            }
        }
    }

    /// Starts ElephantHandler if it is not running; false if it cannot.
    fn start_model(&mut self) -> bool {
        if self.elephant.is_some() {
            return true;
        }
        let Ok((mine, theirs)) = channel_create() else {
            return false;
        };
        match spawn("elephant-handler", Some(theirs)) {
            Ok(process) => {
                let _ = close(process);
                self.elephant = Some(mine);
                true
            }
            Err(_) => {
                let _ = close(mine);
                false
            }
        }
    }

    /// Asks the model for a reply in words (docs/talk-design.md).
    fn ask_talk(&mut self, text: &str) {
        self.talk_with(text, "");
    }

    /// `ask_talk`, with `extra` (what is on the screen) given as context
    /// besides the facts the user told Bella.
    fn talk_with(&mut self, text: &str, extra: &str) {
        if !self.start_model() {
            self.say("Error: cannot start the model service.");
            return;
        }
        self.talking = Some(String::from(text));
        let mut msg = String::from("talk ");
        msg.push_str(text);
        let facts = self.facts_about(text, 1);
        if !facts.is_empty() {
            zoo_rt::log!("Bella: from what you told me: {}", facts);
        }
        let mut context = String::from(extra);
        if !facts.is_empty() {
            if !context.is_empty() {
                context.push_str("; ");
            }
            context.push_str(&facts);
        }
        if !context.is_empty() {
            msg.push('\t');
            msg.push_str(&shorten(context));
        }
        let Some(e) = self.elephant else { return };
        if send_wait(e, msg.as_bytes()).is_err() {
            self.model_ended(false);
            return;
        }
        self.mode = Mode::Thinking;
        self.status = String::from("Thinking... (Backspace cancels)");
    }

    fn ask_model(&mut self, text: &str) {
        if !self.start_model() {
            self.say("Error: cannot start the model service.");
            return;
        }
        let mut msg = String::from("choose ");
        msg.push_str(text);
        // A follow-up ("is that it?") gets the last exchange
        // (docs/talk-design.md §6).
        self.following = false;
        if follow_up(text)
            && let Some(e) = &self.last_exchange
        {
            msg.push('\t');
            msg.push_str(e);
            self.following = true;
            zoo_rt::log!("Bella: a follow-up; with the last exchange");
        }
        let Some(e) = self.elephant else { return };
        if send_wait(e, msg.as_bytes()).is_err() {
            self.elephant = None;
            self.say("Error: the model service stopped.");
            return;
        }
        self.mode = Mode::Thinking;
        self.status = String::from("Thinking... (Backspace cancels)");
    }

    fn model_reply(&mut self, msg: &str) {
        if let Some(text) = msg.strip_prefix("part ") {
            // The reply so far: one transcript line, rewritten as it grows.
            let mut l = String::from("Bella: ");
            l.push_str(text);
            if self.streaming {
                if let Some(last) = self.lines.back_mut() {
                    *last = l;
                }
            } else {
                self.say(&l);
                self.streaming = true;
                zoo_rt::log!(
                    "Bella: first words on screen {} ms after Enter",
                    (zoo_rt::now_us() - self.asked_at) / 1000
                );
            }
            return;
        }
        if let Some(progress) = msg.strip_prefix("busy ") {
            self.status.clear();
            let _ = write!(self.status, "Thinking: {} (Backspace cancels)", progress);
            return;
        }
        self.mode = Mode::Typing;
        self.status = String::from(HINT);
        let summarising = self.summarising.take();
        if !msg.starts_with("say ") {
            self.sources = None;
        }
        let streamed = core::mem::take(&mut self.streaming);
        if let Some(text) = msg.strip_prefix("say ") {
            // A reply in words: shown, never run. It replaces the line
            // written as it came.
            let mut l = String::from("Bella: ");
            l.push_str(text);
            match self.lines.back_mut() {
                Some(last) if streamed => *last = l,
                _ => self.say(&l),
            }
            self.answered("the model, in words");
            // The model keeps its own reply in its conversation; here it is
            // only remembered for a follow-up's command choice.
            if let Some(from) = self.sources.take() {
                let mut l = String::from("  (from ");
                l.push_str(&from);
                l.push(')');
                self.say(&l);
            }
            // A summary was not kept in the model's conversation: note
            // it, without the file's text, so follow-ups know it.
            if let Some(file) = self.summary_of.take()
                && summarising.is_some()
            {
                self.last_summary = Some((file, String::from(text)));
            }
            if let Some(asked) = summarising {
                self.remember_exchange(&asked, text);
            }
            if let Some(asked) = self.talking.take() {
                let mut e = String::from("The user said: ");
                e.push_str(&asked);
                e.push_str(". Bella replied: ");
                e.push_str(text);
                self.last_exchange = Some(shorten(e));
            }
            return;
        }
        if let Some(rest) = msg.strip_prefix("tool ") {
            // `tool <word> <margin> <argument>`
            let mut parts = rest.splitn(3, ' ');
            let word = parts.next().unwrap_or("");
            let _margin = parts.next();
            let arg = parts.next().unwrap_or("").trim();
            // No command fits (docs/stage1-design.md, B9), or the model chose
            // one with nothing to act on: Bella answers in words instead
            // (docs/talk-design.md), and a command typed next can still
            // answer the request.
            // A follow-up's command with nothing to act on is a question
            // about the last answer ("is that it?" chose `list` of home).
            // `list`, `inspect` and `info` act on nothing (home, or the
            // computer); the other commands need something to act on.
            let empty = arg.is_empty()
                && word != "info"
                && word != "split"
                && (self.following || !matches!(word, "list" | "inspect"));
            if word == "none" || empty {
                zoo_rt::log!("Bella: no command fits");
                let request = self.request.take();
                if let Some(r) = &request {
                    let r = r.clone();
                    self.ask_talk(&r);
                } else {
                    self.say("Bella: I can only search, list, inspect or create folders.");
                }
                self.unanswered = request;
                return;
            }
            let mut cmd = String::from(word);
            if !arg.is_empty() {
                cmd.push(' ');
                cmd.push_str(arg);
            }
            let mut l = String::from("Bella: ");
            l.push_str(&cmd);
            self.say(&l);
            let asked = self.request.clone();
            self.command(&cmd);
            self.answered("the model");
            if let Some(a) = asked {
                self.exchanged(&a, &cmd);
            }
        } else {
            // Not answered: a command typed next answers it.
            if let Some(r) = self.request.take() {
                self.unanswered = Some(r);
            }
            let why = msg.strip_prefix("err ").unwrap_or(msg);
            let line = match why {
                "cancelled" => String::from("Cancelled."),
                "model not installed" => {
                    String::from("The model is not installed; type a command instead.")
                }
                other => {
                    let mut l = String::from("Bella: ");
                    l.push_str(other);
                    l
                }
            };
            self.say(&line);
        }
    }
}
