//! Assistant: the Assistant window (docs/m5c2-design.md). The user types
//! either a command (`search …`, `list …`, `inspect …`, `create …`, `undo`),
//! which goes straight to the file broker, or a request in words, which
//! ElephantHandler (the model service) turns into one command first.
//!
//! `create` always shows OtterHandler's plan and waits for `y` before
//! anything changes. Backspace on an empty line cancels a running request.
//!
//! Handles: the window channel (first) and a home directory handle (second),
//! which is moved into OtterHandler when it is started; the Assistant itself never
//! uses it. Both helpers start on first use.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::string::String;
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
const COMMANDS: [&str; 5] = ["search", "list", "inspect", "create", "undo"];
const HINT: &str = "Ask in words, or type search/list/inspect/create/undo.";

enum Mode {
    Typing,
    /// Waiting for the model; the original request is kept for the log.
    Thinking,
    /// A `create` plan is shown; `y` runs it.
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
    };
    if p.win.title("Assistant").is_err() || p.win.lines(ROWS).is_err() || p.draw().is_err() {
        return 0;
    }
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        let mut busy = false;
        // Take every queued key before redrawing, so fast typing is not
        // dropped by a full queue in the window server.
        loop {
            match try_recv(chan, &mut buf) {
                Ok(n) => {
                    busy = true;
                    if let Some(key) = Key::parse(&buf[..n]) {
                        p.key(key);
                    }
                }
                Err(Error::Again) => break,
                Err(_) => return 0,
            }
        }
        if let (Mode::Thinking, Some(e)) = (&p.mode, p.elephant) {
            match try_recv(e, &mut buf) {
                Ok(n) => {
                    busy = true;
                    let msg = String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                    p.model_reply(&msg);
                }
                Err(Error::Again) => {}
                Err(_) => {
                    p.elephant = None;
                    p.mode = Mode::Typing;
                    p.status = String::from("The model service stopped.");
                }
            }
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
                if let Some(e) = self.elephant {
                    let _ = send(e, b"cancel");
                    zoo_rt::log!("Assistant: cancel sent at tick {}", zoo_rt::ticks());
                }
                self.status = String::from("Cancelling...");
            }
            (Mode::Thinking, _) => {}
            (Mode::Confirm(path), Key::Char('y')) => {
                let req = alloc::format!("create {}", path);
                self.mode = Mode::Typing;
                self.status = String::from(HINT);
                self.broker(&req);
            }
            (Mode::Confirm(_), Key::Char('n') | Key::Back) => {
                self.mode = Mode::Typing;
                self.status = String::from(HINT);
                self.say("Not created.");
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
        let word = text.split(' ').next().unwrap_or("");
        if COMMANDS.contains(&word) {
            self.command(text);
        } else {
            self.ask_model(text);
        }
    }

    /// A command, typed or chosen by the model. `create` asks first.
    fn command(&mut self, text: &str) {
        if let Some(path) = text.strip_prefix("create ") {
            let path = String::from(path.trim());
            let mut plan = String::from("plan ");
            plan.push_str(&path);
            if self.broker(&plan) {
                self.status = String::from("Press y to create, n to leave it.");
                self.mode = Mode::Confirm(path);
            }
        } else {
            self.broker(text);
        }
    }

    /// Starts OtterHandler on first use, moving the home handle into it.
    fn otter(&mut self) -> Option<Handle> {
        if self.otter.is_none() && self.home != NO_HANDLE {
            let (mine, theirs) = channel_create().ok()?;
            let home = core::mem::replace(&mut self.home, NO_HANDLE);
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
            self.otter = None;
            self.say("Error: the file broker stopped.");
            return false;
        }
        let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
        loop {
            let Ok(n) = recv(o, &mut buf) else {
                self.otter = None;
                self.say("Error: the file broker stopped.");
                return false;
            };
            let line = core::str::from_utf8(&buf[..n]).unwrap_or("?");
            if let Some(rest) = line.strip_prefix("| ") {
                let mut l = String::from("  ");
                l.push_str(rest);
                self.say(&l);
            } else if line == "ok" {
                return true;
            } else {
                let mut l = String::from("Error: ");
                l.push_str(line.strip_prefix("err ").unwrap_or(line));
                self.say(&l);
                return false;
            }
        }
    }

    fn ask_model(&mut self, text: &str) {
        if self.elephant.is_none() {
            let Ok((mine, theirs)) = channel_create() else {
                self.say("Error: cannot start the model service.");
                return;
            };
            match spawn("elephant-handler", Some(theirs)) {
                Ok(process) => {
                    let _ = close(process);
                    self.elephant = Some(mine);
                }
                Err(_) => {
                    let _ = close(mine);
                    self.say("Error: cannot start the model service.");
                    return;
                }
            }
        }
        let mut msg = String::from("choose ");
        msg.push_str(text);
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
        if let Some(progress) = msg.strip_prefix("busy ") {
            self.status.clear();
            let _ = write!(self.status, "Thinking: {} (Backspace cancels)", progress);
            return;
        }
        self.mode = Mode::Typing;
        self.status = String::from(HINT);
        if let Some(rest) = msg.strip_prefix("tool ") {
            // `tool <word> <margin> <argument>`
            let mut parts = rest.splitn(3, ' ');
            let word = parts.next().unwrap_or("");
            let _margin = parts.next();
            let arg = parts.next().unwrap_or("").trim();
            let mut cmd = String::from(word);
            if !arg.is_empty() {
                cmd.push(' ');
                cmd.push_str(arg);
            }
            let mut l = String::from("Assistant: ");
            l.push_str(&cmd);
            self.say(&l);
            self.command(&cmd);
        } else {
            let why = msg.strip_prefix("err ").unwrap_or(msg);
            let line = match why {
                "cancelled" => String::from("Cancelled."),
                "model not installed" => {
                    String::from("The model is not installed; type a command instead.")
                }
                other => {
                    let mut l = String::from("Assistant: ");
                    l.push_str(other);
                    l
                }
            };
            self.say(&line);
        }
    }
}
