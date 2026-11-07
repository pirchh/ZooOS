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
use zoo_abi::bella::{follow_up, summary_stem};
/// Window server events Bella keeps (docs/events-design.md).
const EVENTS_KEPT: usize = 5;

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
const RECALLABLE: [&str; 17] = [
    "search", "list", "inspect", "create", "read", "open", "move", "trash", "launch", "info",
    "maximize", "minimize", "close", "split", "show", "layout",
    // A routine (docs/routines-design.md).
    "steps",
];

/// A spoken answer: yes (`Some(true)`), no (`Some(false)`), or neither.
fn spoken_answer(text: &str) -> Option<bool> {
    let lower = text.to_ascii_lowercase();
    let first = lower
        .split(|c: char| !c.is_ascii_alphabetic() && c != '\'')
        .find(|w| !w.is_empty())
        .unwrap_or("");
    const YES: [&str; 10] = [
        "yes", "yeah", "yep", "yup", "sure", "ok", "okay", "please", "correct", "right",
    ];
    const NO: [&str; 7] = ["no", "nope", "nah", "cancel", "stop", "don't", "keep"];
    if lower.starts_with("do it") || lower.starts_with("go ahead") || YES.contains(&first) {
        Some(true)
    } else if NO.contains(&first) || lower.starts_with("never mind") {
        Some(false)
    } else {
        None
    }
}

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
    /// An app found several targets (docs/targets-design.md): the
    /// invocation without its target, and the lines to choose from.
    Choose(String, Vec<String>),
}

/// How long a question waits for its answer (docs/targets-design.md): a
/// yes later does not confirm a question asked long ago.
const QUESTION_US: u64 = 60_000_000;
/// After her name alone, how long the pill shows she listens: as long as
/// BatHandler waits for the request (docs/wake-design.md).
const NAME_LISTEN_US: u64 = 5_000_000;
/// The pill shows her working on a spoken request for at least this long,
/// so a quick one is still seen.
const WORKING_MIN_US: u64 = 1_000_000;
/// After a spoken request, how long she listens for a follow-up without
/// her name: as long as BatHandler (docs/follow-design.md).
const FOLLOW_US: u64 = 8_000_000;
/// How often the pill's state is sent again while it is shown: well within
/// the window server's 10 s.
const PILL_AGAIN_US: u64 = 4_000_000;
/// The longest BatHandler takes to write down a request, for the pill.
const HEARING_MAX_US: u64 = 30_000_000;

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
    /// The window server's last few events, newest last
    /// (docs/events-design.md).
    events: VecDeque<String>,
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
    /// BatHandler, her hearing (docs/listening-design.md), while it
    /// listens.
    bat: Option<Handle>,
    /// MynahHandler, her voice (docs/speaking-design.md), once ready.
    mynah: Option<Handle>,
    mynah_ready: bool,
    /// From her first `say:` until MynahHandler says `quiet`: BatHandler
    /// is told, so her own voice isn't taken as speech
    /// (docs/conversation-design.md).
    speaking: bool,
    /// What apps can do for her (docs/registry-design.md).
    registry: zoo_infer::registry::Registry,
    /// The command being run corrects an earlier wrong answer (typed by
    /// the user): it may be learned.
    correction: bool,
    /// A layout sent: the request it was read from (when read from words)
    /// and its tree (docs/layouts-design.md).
    pending_layout: Option<(Option<String>, String)>,
    /// The request whose layout is being asked about.
    layout_request: Option<String>,
    /// The last app action sent: its id and invocation, until answered
    /// (docs/targets-design.md).
    pending_action: Option<(String, String)>,
    /// The titled note the request being handled names ("to my packing
    /// list", docs/titled-design.md), for Notes' actions.
    note_in: Option<String>,
    next_action: u32,
    /// When the open question was asked (`QUESTION_US`).
    asked_question_at: u64,
    /// The pill (docs/wake-design.md): whether the request in hand was
    /// spoken, until when she listens after her name, the least time it
    /// shows her working, and what it shows now.
    spoken: bool,
    name_until: u64,
    working_until: u64,
    pill_shown: &'static str,
    pill_sent_at: u64,
    /// Until when she listens for a follow-up (0: none opened for the
    /// request in hand; docs/follow-design.md).
    follow_until: u64,
    /// The steps of the request just done, for "save that as …"
    /// (docs/routines-design.md), and whether a routine is running (its
    /// steps are not a new request to save).
    done_steps: Vec<String>,
    in_routine: bool,
    /// A request whose layout was not what was meant: the next confirmed
    /// layout answers it too.
    relayout_of: Option<String>,
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
    // Bella is one (docs/one-bella-design.md): a second one would load a
    // second model and fight for the microphone. It leaves at once.
    if zoo_rt::claim("bella").is_err() {
        let _ = zoo_rt::log("Bella: another Bella is running; this one leaves");
        return 0;
    }
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
        events: VecDeque::new(),
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
        bat: None,
        mynah: None,
        mynah_ready: false,
        speaking: false,
        correction: false,
        pending_layout: None,
        layout_request: None,
        pending_action: None,
        note_in: None,
        next_action: 0,
        asked_question_at: 0,
        spoken: false,
        name_until: 0,
        working_until: 0,
        pill_shown: "off",
        pill_sent_at: 0,
        follow_until: 0,
        done_steps: Vec::new(),
        in_routine: false,
        relayout_of: None,
        registry: {
            let (reg, skipped) = zoo_rt::registry::load();
            for s in &skipped {
                zoo_rt::log!("Bella: registry: left out {}", s);
            }
            reg
        },
    };
    if p.win.title("Bella").is_err() || p.win.lines(ROWS).is_err() || p.draw().is_err() {
        return 0;
    }
    // What happens on the desktop, for "it" (docs/events-design.md).
    let _ = p.win.subscribe();
    p.warm();
    // Listening for "hey Bella" (docs/listening-design.md).
    p.start_hearing();
    p.start_voice();
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
                    } else if let Some(ev) = buf[..n].strip_prefix(b"event:") {
                        p.event(core::str::from_utf8(ev).unwrap_or(""));
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
        // MynahHandler, her voice (docs/speaking-design.md).
        if let Some(m) = p.mynah {
            match try_recv(m, &mut buf) {
                Ok(n) => {
                    let msg = core::str::from_utf8(&buf[..n]).unwrap_or("");
                    if msg == "ready" {
                        p.mynah_ready = true;
                        zoo_rt::log!("Bella: her voice is ready");
                    } else if msg == "quiet" {
                        p.done_speaking();
                    } else if let Some(why) = msg.strip_prefix("none:") {
                        zoo_rt::log!("Bella: no voice: {}", why);
                        let _ = close(m);
                        p.mynah = None;
                    }
                }
                Err(Error::Again) => {}
                Err(_) => {
                    let _ = close(m);
                    p.mynah = None;
                    zoo_rt::log!("Bella: her voice ended");
                    p.done_speaking();
                }
            }
        }
        // What BatHandler heard said to Bella (docs/listening-design.md).
        if let Some(b) = p.bat {
            match try_recv(b, &mut buf) {
                Ok(n) => {
                    busy = true;
                    let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                    p.hearing_said(&msg);
                }
                Err(Error::Again) => {}
                Err(_) => {
                    let _ = close(b);
                    p.bat = None;
                    zoo_rt::log!("Bella: her hearing ended");
                }
            }
        }
        // The next step of a request of several, once the last is done
        // (docs/multi-step-design.md).
        if matches!(p.mode, Mode::Typing) && !p.steps.is_empty() {
            busy = true;
            p.next_step();
        }
        p.update_pill();
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
            (Mode::Confirm(_) | Mode::Choose(..), _) if self.question_expired() => {
                // Too late: the question is gone, not answered.
                zoo_rt::log!("Bella: the question expired");
                self.mode = Mode::Typing;
                self.status = String::from(HINT);
                self.say("That question has expired; please ask again.");
            }
            (Mode::Choose(inv, choices), Key::Char(c)) if c.is_ascii_digit() => {
                let k = (c as usize).wrapping_sub('1' as usize);
                if let Some(choice) = choices.get(k).cloned() {
                    let inv = inv.clone();
                    self.choose(&inv, &choice);
                }
            }
            (Mode::Choose(..), Key::Char('n') | Key::Back) => {
                self.mode = Mode::Typing;
                self.status = String::from(HINT);
                zoo_rt::log!("Bella: answered no: Left as it is.");
                self.say("Left as it is.");
            }
            (Mode::Choose(..), _) => {}
            (Mode::Confirm(req), Key::Char('y')) => {
                let req = req.clone();
                self.mode = Mode::Typing;
                self.status = String::from(HINT);
                if let Some(spec) = req.strip_prefix("layout-ok ") {
                    // Confirmed: the request means this layout, learned (and
                    // the one before it, if this corrected it).
                    let cmd = alloc::format!("layout {}", spec);
                    for request in [self.layout_request.take(), self.relayout_of.take()]
                        .into_iter()
                        .flatten()
                    {
                        self.request = Some(request);
                        self.correction = true;
                        self.learn(&cmd);
                    }
                    self.say("Got it; I'll remember that.");
                } else if let Some(rest) = req.strip_prefix("routine ") {
                    // `routine <name>\t<command>`: saved as a phrasing
                    // the user confirmed.
                    let (name, cmd) = rest.split_once('\t').unwrap_or((rest, ""));
                    let msg = alloc::format!("remember {}\t{}", name, cmd);
                    let (name, cmd) = (String::from(name), String::from(cmd));
                    if self.broker(&msg) {
                        zoo_rt::log!("Bella: saved the routine `{}` -> `{}`", name, cmd);
                        self.say(&alloc::format!("Saved. Say \"{}\" to run it.", name));
                    }
                } else if let Some(about) = req.strip_prefix("phrase-forget ") {
                    let msg = alloc::format!("forget-phrase {}", about);
                    if self.broker(&msg) {
                        zoo_rt::log!("Bella: forgot the phrasing `{}`", about);
                        self.say("Forgotten.");
                    }
                } else if let Some(cmd) = req.strip_prefix("unsure ") {
                    // Confirmed: run it, and the request means it.
                    self.correction = self.request.is_some();
                    let asked = self.request.clone();
                    self.command(cmd);
                    if let Some(a) = asked {
                        self.exchanged(&a, cmd);
                    }
                } else if let Some(inv) = req.strip_prefix("app ") {
                    // A registered action, asked about (`app_action`). Not
                    // remembered: its words change with every request.
                    self.request = None;
                    self.invoke(inv);
                } else if self.broker(&req) {
                    self.learn(&req);
                }
            }
            (Mode::Confirm(req), Key::Char('n') | Key::Back) if req.starts_with("unsure ") => {
                // Not what was meant: answered in words instead, and a
                // command typed next can still answer the request.
                self.drop_steps();
                self.mode = Mode::Typing;
                self.status = String::from(HINT);
                zoo_rt::log!("Bella: answered no: Okay, I won't.");
                self.say("Okay, I won't.");
                self.speak("Okay, I won't.");
                if let Some(r) = self.request.take() {
                    self.ask_talk(&r);
                    self.unanswered = Some(r);
                }
            }
            (Mode::Confirm(req), Key::Char('n') | Key::Back) => {
                let what = if req.starts_with("create ") {
                    "Not created."
                } else if req.starts_with("layout-ok ") {
                    // Not what was meant: the next request says how, and
                    // answers this one too.
                    self.relayout_of = self.layout_request.take();
                    self.await_answer();
                    "How should they go? Say rows, columns, quarters, or two on top and one below."
                } else if req.starts_with("fact-forget ")
                    || req.starts_with("app ")
                    || req.starts_with("phrase-forget ")
                {
                    "Kept."
                } else if req.starts_with("routine ") {
                    "Not saved."
                } else {
                    "Left where it is."
                };
                self.drop_steps();
                self.mode = Mode::Typing;
                self.status = String::from(HINT);
                zoo_rt::log!("Bella: answered no: {}", what);
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
                // Typed: the pill is for spoken requests.
                self.spoken = false;
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
        use zoo_infer::route::Plan;
        // The address ("hey Bella") off, "maximize it" after the user
        // opened Notes resolved from the desktop's events
        // (docs/events-design.md), then the steps (docs/regression-design.md:
        // the host's `bella-regress` takes the same path).
        // A new request: whatever routine ran before is over.
        self.in_routine = false;
        match zoo_infer::route::plan(text, |t| self.it_from_events(t)) {
            Plan::Negated => {
                // "Don't delete the milk entry" (docs/targets-design.md).
                zoo_rt::log!("Bella: a request not to: nothing done");
                self.say("Okay, I won't.");
            }
            Plan::Routine(name, steps) => self.offer_routine(&name, &steps),
            Plan::SaveAs(name) => {
                let steps = core::mem::take(&mut self.done_steps);
                if steps.is_empty() {
                    self.say("There is nothing to save yet: ask me something first.");
                } else {
                    self.offer_routine(&name, &steps);
                }
            }
            Plan::Steps(steps) => {
                // Each runs on its own, in order (docs/multi-step-design.md).
                self.done_steps.clear();
                zoo_rt::log!("Bella: {} steps: {}", steps.len(), steps.join(" | "));
                self.steps = steps.into();
                self.last_step.clear();
                self.next_step();
            }
            Plan::One(text) => {
                self.last_step.clear();
                self.done_steps.clear();
                self.done_steps.push(text.clone());
                self.submit_one(&text);
            }
        }
    }

    /// One of the window server's events: kept, the last few.
    fn event(&mut self, what: &str) {
        if self.events.len() == EVENTS_KEPT {
            self.events.pop_front();
        }
        self.events.push_back(String::from(what));
    }

    /// `text` with "it", "that" or "them" naming the app of the newest
    /// event that names one other than Bella, for window commands only
    /// ("open it" after a search still means the file found). Unchanged
    /// otherwise.
    fn it_from_events(&self, text: &str) -> String {
        const WINDOW_VERBS: [&str; 10] = [
            "maximize", "maximise", "minimize", "minimise", "close", "quit", "hide", "split",
            "tile", "show",
        ];
        let first = text.split_whitespace().next().unwrap_or("");
        if !WINDOW_VERBS.iter().any(|v| v.eq_ignore_ascii_case(first))
            || !zoo_infer::assist::apps_named(text, false).is_empty()
        {
            return String::from(text);
        }
        let Some(app) = self
            .events
            .iter()
            .rev()
            .find_map(|e| zoo_infer::assist::apps_named(e, false).first().copied())
        else {
            return String::from(text);
        };
        let resolved = zoo_infer::assist::resolve_it(text, app);
        if resolved != text {
            zoo_rt::log!("Bella: \"it\" is {} (from the desktop's events)", app);
        }
        resolved
    }

    /// Runs the next step, with "it" naming the apps of the one before.
    fn next_step(&mut self) {
        let Some(step) = self.steps.pop_front() else {
            return;
        };
        let step = zoo_infer::assist::resolve_it(&step, &self.last_step);
        zoo_rt::log!("Bella: step: {}", step);
        if !self.in_routine {
            self.done_steps.push(step.clone());
        }
        self.last_step = step.clone();
        self.submit_one(&step);
    }

    fn submit_one(&mut self, text: &str) {
        use zoo_infer::route::{Known, Route};
        self.asked_at = zoo_rt::now_us();
        // "Add socks to my packing list": the title kept for Notes, the
        // rest read as "add socks to my list" (docs/titled-design.md).
        self.note_in = None;
        let titled = zoo_infer::notes::titled_place(text);
        let text: &str = match &titled {
            Some((title, rest)) => {
                zoo_rt::log!("Bella: names the note \"{}\"", title);
                self.note_in = Some(title.clone());
                rest
            }
            None => text,
        };
        let known = Known {
            relayout: self.relayout_of.is_some(),
            summary: self.last_summary.is_some(),
            exchange: self.last_exchange.is_some(),
        };
        let route = zoo_infer::route::route(text, &self.registry, known);
        match route {
            Route::Typed => {
                // Typed by the user: it may answer an earlier request. A
                // step of a request of several is not one: "maximize Notes"
                // after "Open notes" corrects nothing (docs/wake-design.md).
                if self.last_step.is_empty() {
                    self.request = self.unanswered.take();
                } else {
                    self.request = None;
                }
                // A typed command after a wrong answer: the user's own
                // correction, the one thing she learns from for now.
                self.correction = self.request.is_some();
                // The command word as the commands spell it ("Open notes",
                // as speech to text writes it, is `open notes`).
                let (word, rest) = text.split_once(' ').unwrap_or((text, ""));
                let mut cmd = word.to_ascii_lowercase();
                if !rest.is_empty() {
                    cmd.push(' ');
                    cmd.push_str(rest);
                }
                self.command(&cmd);
                self.answered("a typed command");
                self.exchanged(text, &cmd);
            }
            Route::Layout(tree) => {
                // A layout in words (docs/layouts-design.md), read here
                // without the model; asked about once arranged. A phrasing
                // she learned: from memory, no question.
                self.request = Some(String::from(text));
                if self.recall(text) {
                    self.answered("memory");
                } else {
                    zoo_rt::log!("Bella: read the layout {} from the words", tree.text());
                    let cmd = alloc::format!("layout {}", tree.text());
                    self.command(&cmd);
                }
            }
            Route::Phrased(cmd) => {
                // In the app's own registered words: as if the model had
                // chosen it (docs/regression-design.md).
                zoo_rt::log!("Bella: in the registry's words: {}", cmd);
                self.request = Some(String::from(text));
                self.say(&alloc::format!("Bella: {}", cmd));
                self.command(&cmd);
                self.answered("the registry's words");
                self.exchanged(text, &cmd);
            }
            Route::Sides(cmd) => {
                // Two apps, left and right (docs/refine-design.md).
                zoo_rt::log!("Bella: left and right: {}", cmd);
                self.request = Some(String::from(text));
                self.say(&alloc::format!("Bella: {}", cmd));
                self.command(&cmd);
                self.answered("left and right");
                self.exchanged(text, &cmd);
            }
            Route::SaveSummary => {
                self.request = None;
                self.save_summary();
            }
            Route::MyFiles => {
                self.request = None;
                self.answer_from_files(text);
            }
            Route::Summarise(path) => {
                self.request = None;
                self.summarise(text, path);
            }
            Route::Activity => {
                // What is on the screen: asked of the window server,
                // answered in words (docs/context-design.md).
                self.request = None;
                self.ack(&LOOKING);
                if self.win.context().is_ok() {
                    self.asking_context = Some(String::from(text));
                    self.mode = Mode::Thinking;
                    self.status = String::from("Looking at the screen...");
                }
            }
            Route::Remember(fact) => {
                // Facts the user tells Bella (docs/facts-design.md).
                let mut req = String::from("fact-add ");
                req.push_str(fact);
                if self.broker(&req) {
                    self.say("Bella: I'll remember that.");
                    zoo_rt::log!("Bella: remembered a fact");
                }
                self.request = None;
            }
            Route::WhatDid => {
                self.say("Bella: What I changed, newest last:");
                if !self.broker("audit-list") {
                    self.say("  nothing yet.");
                }
                self.request = None;
            }
            Route::WhatRemembered => {
                self.say("Bella: You told me:");
                if !self.broker("fact-list") {
                    self.say("  nothing yet.");
                }
                self.request = None;
            }
            Route::Forget(about) if self.remembered(about).is_some_and(|(s, _)| s == 100) => {
                // A phrasing or routine she remembers, these very words
                // (docs/templates-design.md): asked first, like any delete.
                let cmd = self.remembered(about).map(|r| r.1).unwrap_or_default();
                let q = alloc::format!("Forget that \"{}\" means `{}`?", about, cmd);
                zoo_rt::log!("Bella: asks: {}", q);
                self.say(&q);
                self.speak(&q);
                self.status = String::from("Press y to forget it, n to keep it.");
                self.asked_question_at = zoo_rt::now_us();
                self.mode = Mode::Confirm(alloc::format!("phrase-forget {}", about));
                self.await_answer();
                self.request = None;
            }
            Route::Forget(about) => {
                // Forgetting is deleting: ask first (docs/access-design.md).
                let mut q = String::from("Forget what you told me about ");
                q.push_str(about);
                q.push('?');
                self.say(&q);
                self.status = String::from("Press y to forget it, n to keep it.");
                self.asked_question_at = zoo_rt::now_us();
                self.mode = Mode::Confirm(alloc::format!("fact-forget {}", about));
                self.request = None;
            }
            Route::AboutAnswer => {
                // "is that it?": answered in words from the conversation
                // (docs/talk-design.md §6).
                zoo_rt::log!("Bella: a question about the last answer");
                self.request = None;
                self.ask_talk(text);
            }
            Route::SmallTalk => {
                self.request = None;
                self.ask_talk(text);
            }
            Route::Model if !self.facts_about(text, 2).is_empty() => {
                // Something the user told Bella answers it ("when is my
                // dentist?"): a reply in words with those facts.
                self.request = None;
                self.ask_talk(text);
            }
            Route::Model => {
                self.request = Some(String::from(text));
                if self.recall(text) {
                    self.answered("memory");
                } else {
                    self.ask_model(text);
                }
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
        // A routine (docs/routines-design.md): its steps, run in order.
        if let Some(body) = text.strip_prefix("steps ") {
            let steps: VecDeque<String> = body
                .split(" | ")
                .map(|s| String::from(s.trim()))
                .filter(|s| !s.is_empty())
                .collect();
            zoo_rt::log!("Bella: routine: {} steps", steps.len());
            self.steps = steps;
            self.last_step.clear();
            self.in_routine = true;
            return;
        }
        // Words instead of an action: what she cannot do yet, or what she
        // needs to know (docs/one-bella-design.md).
        // Windows arranged by a layout tree (docs/layouts-design.md); one
        // she read from words is checked with the user once it is done.
        if let Some(spec) = text.strip_prefix("layout ") {
            let spec = spec.trim();
            if zoo_infer::layout::parse(spec).is_none() {
                self.say(&alloc::format!("Error: `{}` is not a layout.", spec));
                self.request = None;
                return;
            }
            if self.win.layout(spec).is_ok() {
                zoo_rt::log!("Bella: asked for the layout {}", spec);
                let interpreted = self.request.take();
                self.pending_layout = Some((interpreted, String::from(spec)));
            }
            return;
        }
        if let Some(said) = text.strip_prefix("say ") {
            zoo_rt::log!("Bella: says: {}", said);
            self.say(said);
            self.request = None;
            return;
        }
        if let Some(path) = text.strip_prefix("create ") {
            let path = String::from(path.trim());
            let mut plan = String::from("plan ");
            plan.push_str(&path);
            if self.broker(&plan) {
                self.status = String::from("Press y to create, n to leave it.");
                self.asked_question_at = zoo_rt::now_us();
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
                self.asked_question_at = zoo_rt::now_us();
                self.mode = Mode::Confirm(alloc::format!("trash {}", real));
            }
        } else if let Some(path) = text.strip_prefix("open ") {
            self.open(path.trim());
        } else if let Some(action) = self
            .registry
            .find_word(text.split(' ').next().unwrap_or(""))
            .cloned()
        {
            // A registered app action (docs/registry-design.md).
            self.app_action(&action, text);
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
        // An app's structured answer (docs/targets-design.md), or a plain
        // sentence from the window server.
        let outcome = zoo_infer::registry::Outcome::parse(said);
        let said = outcome.said.as_str();
        zoo_rt::log!("Bella: the window server says: {}", said);
        self.say(said);
        self.speak(said);
        let ours = match (&outcome.id, &self.pending_action) {
            (Some(id), Some((p, _))) => id == p,
            _ => false,
        };
        // Answered, or refused by the window server (a sentence with no
        // id): nothing of hers waits on an app now.
        if (ours && outcome.status != zoo_infer::registry::Status::Choose) || outcome.id.is_none() {
            self.pending_action = None;
        }
        if ours
            && outcome.status == zoo_infer::registry::Status::Choose
            && let Some((_, inv)) = self.pending_action.take()
        {
            let list: Vec<String> = outcome
                .choices
                .iter()
                .enumerate()
                .map(|(k, c)| alloc::format!("{}. {}", k + 1, c))
                .collect();
            zoo_rt::log!("Bella: asks: Which one: {}", list.join(" | "));
            for l in &list {
                self.say(l);
            }
            self.status = String::from("Press a number, or say which; n leaves it.");
            self.asked_question_at = zoo_rt::now_us();
            self.mode = Mode::Choose(inv, outcome.choices.clone());
            self.await_answer();
            let _ = self.draw();
            return;
        }
        // A layout she read from words: is it what was meant?
        if let Some((Some(request), spec)) = self.pending_layout.take()
            && said.starts_with("Arranged ")
        {
            zoo_rt::log!("Bella: asks: Is this what you meant?");
            self.say("Is this what you meant?");
            self.status = String::from("Say yes or press y to keep it; no or n to say how.");
            self.layout_request = Some(request);
            self.asked_question_at = zoo_rt::now_us();
            self.mode = Mode::Confirm(alloc::format!("layout-ok {}", spec));
            self.await_answer();
        }
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
        // Only what the user confirmed (docs/one-bella-design.md): her own
        // choices taught her wrong mappings in the owner's demo.
        if !core::mem::take(&mut self.correction) {
            zoo_rt::log!(
                "Bella: not learned `{}` -> `{}` (not confirmed)",
                request,
                command
            );
            return;
        }
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
    /// What memory holds for `text`: the likeness (percent) and command
    /// of the most alike remembered request, if any.
    fn remembered(&mut self, text: &str) -> Option<(usize, String)> {
        let o = self.otter()?;
        let msg = alloc::format!("recall {}", text);
        send_wait(o, msg.as_bytes()).ok()?;
        let mut best: Option<(usize, String)> = None;
        let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
        loop {
            let Ok(n) = recv(o, &mut buf) else {
                self.otter = None;
                return None;
            };
            let line = core::str::from_utf8(&buf[..n]).unwrap_or("");
            if let Some(rest) = line.strip_prefix("| ") {
                let (score, cmd) = rest.split_once(' ').unwrap_or((rest, ""));
                best = score.parse().ok().map(|s| (s, String::from(cmd)));
            } else {
                break;
            }
        }
        best
    }

    fn recall(&mut self, text: &str) -> bool {
        let Some((score, cmd)) = self.remembered(text).filter(|(s, _)| *s >= RECALL_AT) else {
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

    /// Starts MynahHandler, her voice, when a voice model is installed
    /// (the boot image or the model disk).
    fn start_voice(&mut self) {
        let installed = zoo_rt::fs_find(zoo_abi::SPEAK_MODEL_FILE).is_some()
            || zoo_rt::model_size_of(zoo_abi::MODEL_SPEAK).is_ok();
        if !installed {
            return;
        }
        let Ok((mine, theirs)) = channel_create() else {
            return;
        };
        match spawn("mynah-handler", Some(theirs)) {
            Ok(process) => {
                let _ = close(process);
                self.mynah = Some(mine);
            }
            Err(_) => {
                let _ = close(mine);
            }
        }
    }

    /// Says `text` aloud, when the request was spoken and her voice is
    /// ready (docs/speaking-design.md). Typed requests stay silent.
    fn speak(&mut self, text: &str) {
        if !self.spoken || !self.mynah_ready {
            return;
        }
        if let Some(m) = self.mynah {
            let mut msg = String::from("say:");
            msg.push_str(text);
            if zoo_rt::send(m, msg.as_bytes()).is_ok() {
                zoo_rt::log!("Bella: says aloud: {}", text);
                if !self.speaking {
                    self.speaking = true;
                    if let Some(b) = self.bat {
                        let _ = send(b, b"mute");
                    }
                }
            }
        }
    }

    /// MynahHandler has nothing left to say: BatHandler listens to
    /// everything again, and a follow-up window opens from now.
    fn done_speaking(&mut self) {
        if !self.speaking {
            return;
        }
        self.speaking = false;
        zoo_rt::log!("Bella: done speaking");
        if let Some(b) = self.bat {
            let _ = send(b, b"unmute");
        }
    }

    /// Starts BatHandler, her hearing, when a hearing model is installed
    /// (the boot image or the model disk); it says `none:` and ends when
    /// there is no microphone.
    fn start_hearing(&mut self) {
        let installed = zoo_rt::fs_find(zoo_abi::HEAR_MODEL_FILE).is_some()
            || zoo_rt::model_size_of(zoo_abi::MODEL_HEAR).is_ok();
        if !installed {
            return;
        }
        let Ok((mine, theirs)) = channel_create() else {
            return;
        };
        match spawn("bat-handler", Some(theirs)) {
            Ok(process) => {
                let _ = close(process);
                self.bat = Some(mine);
            }
            Err(_) => {
                let _ = close(mine);
            }
        }
    }

    /// A message from BatHandler: a request said to her, as if typed.
    fn hearing_said(&mut self, msg: &str) {
        if msg == "listening" {
            zoo_rt::log!("Bella: listening for \"hey Bella\"");
        } else if msg == "name" {
            // Her name alone: the request follows.
            zoo_rt::log!("Bella: heard her name; listening for the request");
            self.spoken = true;
            self.name_until = zoo_rt::now_us() + NAME_LISTEN_US;
        } else if msg == "hearing" {
            // BatHandler is writing down what followed her name: she works
            // on it (the pill), until it says what it heard.
            self.spoken = true;
            self.name_until = 0;
            self.working_until = zoo_rt::now_us() + HEARING_MAX_US;
        } else if let Some(text) = msg.strip_prefix("follow:") {
            // Said after her answer, without her name
            // (docs/follow-design.md): hers only if it is something she
            // does without the model; otherwise not for her, and its words
            // stay out of the log.
            let text = text.trim().trim_end_matches(['.', '!', ',', ';']);
            // Decided here alone: asking memory would put the words in
            // OtterHandler's audit log before they are known to be hers.
            let ok = matches!(self.mode, Mode::Typing)
                && self.steps.is_empty()
                && zoo_infer::route::needs_no_model(text, &self.registry);
            if ok {
                zoo_rt::log!("Bella: heard \"{}\" as a follow-up", text);
                let mut echo = String::from("You said: ");
                echo.push_str(text);
                self.say(&echo);
                self.spoken = true;
                self.follow_until = 0;
                self.working_until = zoo_rt::now_us() + WORKING_MIN_US;
                self.submit(&String::from(text));
            } else {
                zoo_rt::log!(
                    "Bella: speech after her answer, not a request ({} words)",
                    text.split_whitespace().count()
                );
                self.working_until = 0;
                // The window is used: the pill goes, none reopens.
                self.follow_until = 1;
            }
        } else if let Some(text) = msg.strip_prefix("barge:") {
            // Said to her while she spoke (docs/conversation-design.md):
            // she stops, then takes it as she would have.
            if self.speaking
                && let Some(m) = self.mynah
            {
                let _ = send(m, b"stop");
                zoo_rt::log!("Bella: interrupted");
            }
            self.spoken = true;
            if zoo_abi::bella::stop_talking(text) {
                zoo_rt::log!("Bella: stops talking, as asked");
                self.working_until = 0;
            } else if zoo_abi::bella::name_only(text) {
                zoo_rt::log!("Bella: heard her name; listening for the request");
                self.name_until = zoo_rt::now_us() + NAME_LISTEN_US;
            } else {
                let mut heard = String::from("heard:");
                heard.push_str(text);
                self.hearing_said(&heard);
            }
        } else if msg == "lost" {
            // Nothing could be made of it.
            self.working_until = 0;
        } else if let Some(why) = msg.strip_prefix("none:") {
            zoo_rt::log!("Bella: not listening: {}", why);
            if let Some(b) = self.bat.take() {
                let _ = close(b);
            }
        } else if let Some(text) = msg.strip_prefix("heard:") {
            // Spoken sentences end in a mark the typed ones lack (".", ",").
            let text = text.trim().trim_end_matches(['.', '!', ',', ';']);
            zoo_rt::log!("Bella: heard \"{}\"", text);
            self.spoken = true;
            self.name_until = 0;
            self.working_until = zoo_rt::now_us() + WORKING_MIN_US;
            let mut echo = String::from("You said: ");
            echo.push_str(text);
            self.say(&echo);
            if let Mode::Choose(inv, choices) = &self.mode {
                // Which one, spoken: "the first", "two", or words of a line.
                let (inv, choices) = (inv.clone(), choices.clone());
                if self.question_expired() {
                    self.key(Key::Back);
                } else if let Some(k) = zoo_abi::bella::spoken_choice(
                    text,
                    &choices.iter().map(String::as_str).collect::<Vec<_>>(),
                ) {
                    self.choose(&inv, &choices[k]);
                } else if spoken_answer(text) == Some(false) {
                    self.key(Key::Back);
                } else {
                    self.say("Which one? Say the first, the second, or words from the line.");
                    self.await_answer();
                }
            } else if matches!(self.mode, Mode::Confirm(_)) {
                // The answer to her question, spoken.
                match spoken_answer(text) {
                    Some(true) => self.key(Key::Char('y')),
                    Some(false) => self.key(Key::Char('n')),
                    None => {
                        self.say("Say yes or no.");
                        self.await_answer();
                    }
                }
            } else if matches!(self.mode, Mode::Typing) && self.steps.is_empty() {
                self.submit(&String::from(text));
            } else {
                self.say("I heard you, but I am still working on the last request.");
            }
        }
    }

    /// `text` as the registered action `action` (docs/registry-design.md):
    /// its slots are the model's (already joined) or filled from `text`'s
    /// own words; a delete is asked about first, anything else is sent.
    fn app_action(&mut self, action: &zoo_infer::registry::Action, text: &str) {
        use zoo_infer::registry::{Effect, SLOT_SEP};
        let rest = text[action.word.len()..].trim();
        let filled = if rest.contains(SLOT_SEP) {
            String::from(rest)
        } else {
            action.fill(text)
        };
        if filled.is_empty() {
            self.say(&alloc::format!(
                "Error: I could not tell what to {} from that.",
                action.about
            ));
            return;
        }
        let mut inv = alloc::format!("{}:{}:{}", action.program, action.name, filled);
        let title = if action.program == "notes" {
            self.note_in.take()
        } else {
            None
        };
        if let Some(title) = &title {
            inv.push(SLOT_SEP);
            inv.push('^');
            inv.push_str(title);
        }
        if action.effect == Effect::Delete {
            self.asked_question_at = zoo_rt::now_us();
            let mut q = action.question(&filled);
            if let Some(title) = &title {
                q = q.replace("your notes", &alloc::format!("your {} note", title));
            }
            zoo_rt::log!("Bella: asks: {}", q);
            self.say(&q);
            self.speak(&q);
            self.status = String::from("Say yes or press y to go ahead; no or n leaves it.");
            self.asked_question_at = zoo_rt::now_us();
            self.mode = Mode::Confirm(alloc::format!("app {}", inv));
            self.await_answer();
        } else {
            self.invoke(&inv);
        }
    }

    /// `inv` again with the chosen target (docs/targets-design.md).
    fn choose(&mut self, inv: &str, choice: &str) {
        zoo_rt::log!("Bella: chose \"{}\"", choice);
        self.mode = Mode::Typing;
        self.status = String::from(HINT);
        let with = alloc::format!("{}{}@{}", inv, zoo_infer::registry::SLOT_SEP, choice);
        self.invoke(&with);
    }

    /// Whether the question asked is too old to answer.
    fn question_expired(&self) -> bool {
        zoo_rt::now_us().saturating_sub(self.asked_question_at) > QUESTION_US
    }

    /// Sends `<program>:<action>:<slots>` through the window server, with
    /// a request id its answer carries back (docs/targets-design.md).
    fn invoke(&mut self, inv: &str) {
        self.next_action += 1;
        let id = self.next_action;
        self.pending_action = Some((alloc::format!("{id}"), String::from(inv)));
        let inv = alloc::format!("{}{}#{}", inv, zoo_infer::registry::SLOT_SEP, id);
        let mut parts = inv.splitn(3, ':');
        let (Some(program), Some(name), Some(arg)) = (parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        if self.win.act(program, name, arg).is_ok() {
            zoo_rt::log!("Bella: asked {} to {}", program, name);
        } else {
            self.say("Error: the window server did not take that.");
        }
    }

    /// Asks BatHandler to pass on the next thing said, without "hey Bella"
    /// (the answer to her question).
    /// Asks before saving a routine (docs/routines-design.md): its name
    /// and its steps, read back.
    fn offer_routine(&mut self, name: &str, steps: &[String]) {
        let Some(cmd) = zoo_infer::route::routine_command(steps) else {
            self.say("Error: I can't save those steps as a routine.");
            return;
        };
        let q = alloc::format!("Save \"{}\" as: {}?", name, steps.join(" | "));
        zoo_rt::log!("Bella: asks: {}", q);
        self.say(&q);
        self.speak(&q);
        self.status = String::from("Say yes or press y to save it; no or n doesn't.");
        self.asked_question_at = zoo_rt::now_us();
        self.mode = Mode::Confirm(alloc::format!("routine {}\t{}", name, cmd));
        self.await_answer();
    }

    /// What the pill should show (docs/wake-design.md), sent to the
    /// window server when it changes. Only for spoken requests, and only
    /// while she can hear.
    fn update_pill(&mut self) {
        let now = zoo_rt::now_us();
        let busy = !self.steps.is_empty() || self.pending_action.is_some();
        let mut want = if !self.spoken || self.bat.is_none() {
            "off"
        } else {
            match self.mode {
                Mode::Confirm(_) | Mode::Choose(..) => "listening",
                Mode::Thinking => "working",
                Mode::Typing if busy || self.speaking || now < self.working_until => "working",
                Mode::Typing if now < self.name_until => "listening",
                Mode::Typing => "off",
            }
        };
        // A spoken request done: listen for a follow-up, once.
        if want == "off"
            && self.spoken
            && self.follow_until == 0
            && let Some(b) = self.bat
        {
            let _ = send(b, b"follow");
            self.follow_until = now + FOLLOW_US;
            zoo_rt::log!("Bella: listening for a follow-up");
        }
        if want == "off" && now < self.follow_until && self.spoken {
            want = "listening";
        }
        if want == "off" {
            self.spoken = false;
            self.follow_until = 0;
        }
        // Sent on a change, and again every few seconds while shown: the
        // window server takes the pill down when it hears nothing for
        // longer (Bella gone), not while a slow request runs.
        let again = want != "off" && now.saturating_sub(self.pill_sent_at) >= PILL_AGAIN_US;
        if want != self.pill_shown || again {
            self.pill_shown = want;
            self.pill_sent_at = now;
            let _ = self.win.pill(want);
        }
    }

    fn await_answer(&mut self) {
        if let Some(b) = self.bat {
            let _ = send(b, b"await");
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
            self.speak(text);
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
            let margin = parts
                .next()
                .and_then(|m| m.parse::<i32>().ok())
                .unwrap_or(0);
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
            // A move or close of "that" or to "somewhere else" names
            // nothing to act on (docs/vague-design.md): answered in words.
            let vague = zoo_infer::route::vague(word, arg);
            if vague {
                zoo_rt::log!("Bella: `{}` names nothing to act on", word);
            }
            if word == "none" || empty || vague {
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
            if zoo_infer::route::ask_first(word, margin, &self.registry) {
                // Unsure, and it changes things: asked, not done
                // (docs/unsure-design.md). Yes runs it and teaches her the
                // phrasing; no answers in words.
                zoo_rt::log!(
                    "Bella: unsure ({}.{:02}): asks before `{}`",
                    margin / 100,
                    margin % 100,
                    cmd
                );
                let q = alloc::format!("Do you want me to {}?", cmd);
                zoo_rt::log!("Bella: asks: {}", q);
                self.say(&q);
                self.speak(&q);
                self.status = String::from("Say yes or press y to go ahead; no or n leaves it.");
                self.asked_question_at = zoo_rt::now_us();
                self.mode = Mode::Confirm(alloc::format!("unsure {}", cmd));
                self.await_answer();
                return;
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
