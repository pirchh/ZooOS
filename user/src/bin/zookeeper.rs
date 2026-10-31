//! Zookeeper: the ZooOS supervisor (docs/m3-design.md §9). In M3 it runs a
//! fixed start-up plan: start BelugaTest and talk to it, supervise
//! LemmingTest with a one-restart policy, confirm BelugaTest survived,
//! then stop everything. Each failed expectation exits with its own code.
//!
//! Started by the kernel with a control channel it runs instead as the
//! desktop session (`session`) or as the resident service supervisor
//! (`supervise`, docs/s1-design.md).

#![no_std]
#![no_main]

extern crate alloc;

use zoo_rt::{Error, Handle, channel_create, close, kill, log, recv, send, spawn, wait};

zoo_rt::entry!(main);

/// LemmingTest runs once, then is restarted at most this many times.
const LEMMING_RESTARTS: u32 = 1;

/// PeacockHandler may be restarted this many times in one session.
const DESKTOP_RESTARTS: u32 = 3;

fn main(start_handle: u64) -> u8 {
    // Started with a control channel: its first message says what to do.
    if start_handle != zoo_rt::NO_HANDLE {
        let mut mode = [0u8; 32];
        return match recv(start_handle, &mut mode) {
            Ok(n) if &mode[..n] == b"session" => session(),
            Ok(n) if &mode[..n] == b"supervise" => supervise(start_handle),
            _ => 30,
        };
    }
    startup_checks()
}

/// Desktop session (docs/m4-design.md §3): run the window server, restart it
/// if it ends abnormally, finish when it exits cleanly.
fn session() -> u8 {
    use zoo_rt::dir_rights as r;
    let _ = log("Zookeeper: starting the desktop session");
    // The desktop gets the user's home folder, nothing above it
    // (docs/m5c2-design.md §2).
    let home = match zoo_rt::fs_root() {
        Ok(root) => {
            let home = zoo_rt::dir_open(root, "home/user", r::ALL).ok();
            let _ = close(root);
            home
        }
        Err(_) => None,
    };
    let mut restarts = 0;
    loop {
        let copy = home.and_then(|h| zoo_rt::dir_open(h, "", r::ALL).ok());
        let Ok(peacock) = zoo_rt::spawn_with("peacock-handler", [None, copy]) else {
            return 2;
        };
        let outcome = wait(peacock);
        let _ = close(peacock);
        if outcome == Ok(0) {
            let _ = log("Zookeeper: desktop session ended");
            return 0;
        }
        if restarts == DESKTOP_RESTARTS {
            zoo_rt::log!(
                "Zookeeper: PeacockHandler ended ({:?}); restart limit reached",
                outcome
            );
            return 1;
        }
        restarts += 1;
        zoo_rt::log!(
            "Zookeeper: PeacockHandler ended ({:?}); restarting ({} of {})",
            outcome,
            restarts,
            DESKTOP_RESTARTS
        );
    }
}

/// The M3 start-up plan (docs/m3-design.md §9).
fn startup_checks() -> u8 {
    let abi = zoo_rt::abi_version();
    zoo_rt::log!("Zookeeper starting (kernel ABI {})", abi);
    if abi != zoo_abi::ABI_VERSION {
        return 10;
    }

    let Ok((mine, belugas)) = channel_create() else {
        return 11;
    };
    let Ok(beluga) = spawn("beluga-test", Some(belugas)) else {
        return 12;
    };
    if !ping(mine, "ping 1") {
        return 13;
    }

    for run in 1..=LEMMING_RESTARTS + 1 {
        let Ok(lemming) = spawn("lemming-test", None) else {
            return 14;
        };
        match wait(lemming) {
            Err(Error::Crashed) => {
                zoo_rt::log!(
                    "LemmingTest crashed (run {} of {})",
                    run,
                    LEMMING_RESTARTS + 1
                )
            }
            other => {
                zoo_rt::log!("LemmingTest did not crash: {:?}", other);
                return 15;
            }
        }
        let _ = close(lemming);
        if run <= LEMMING_RESTARTS {
            let _ = log("restart policy allows it: restarting LemmingTest");
        } else {
            let _ = log("restart limit reached: giving up on LemmingTest");
        }
    }

    // MuskoxTest never yields. Talking to BelugaTest while it runs is
    // only possible if the kernel preempts it.
    let Ok(muskox) = spawn("muskox-test", None) else {
        return 19;
    };
    // BelugaTest must have survived LemmingTest's crashes.
    if !ping(mine, "ping 2") {
        return 16;
    }
    if kill(muskox).is_err() {
        return 20;
    }
    match wait(muskox) {
        Err(Error::Killed) => {
            let _ = log("MuskoxTest stopped by the supervisor");
        }
        other => {
            zoo_rt::log!("MuskoxTest did not end as killed: {:?}", other);
            return 21;
        }
    }
    let _ = close(muskox);

    // M5a: file mapping, and per-process floating-point state.
    if let Err(code) = check_file_mapping() {
        return code;
    }
    if let Err(code) = check_narwhals() {
        return code;
    }
    // M5c-1: directory capabilities and the action broker.
    if let Err(code) = check_directories() {
        return code;
    }
    // POSIX step 1: threads, futexes, thread-local storage.
    match spawn("caribou-test", None).map(|h| (wait(h), h)) {
        Ok((Ok(0), h)) => {
            let _ = close(h);
        }
        Ok((other, _)) => {
            zoo_rt::log!("CaribouTest did not pass: {:?}", other);
            return 29;
        }
        Err(_) => return 29,
    }
    // POSIX step 2: address-space reservations and protection.
    match spawn("walrus-test", None).map(|h| (wait(h), h)) {
        Ok((Ok(0), h)) => {
            let _ = close(h);
        }
        Ok((other, _)) => {
            zoo_rt::log!("WalrusTest did not pass: {:?}", other);
            return 31;
        }
        Err(_) => return 31,
    }
    // POSIX step 3: exception delivery to the program's handler.
    match spawn("wolverine-test", None).map(|h| (wait(h), h)) {
        Ok((Ok(0), h)) => {
            let _ = close(h);
        }
        Ok((other, _)) => {
            zoo_rt::log!("WolverineTest did not pass: {:?}", other);
            return 32;
        }
        Err(_) => return 32,
    }
    if send(mine, b"stop").is_err() {
        return 17;
    }
    match wait(beluga) {
        Ok(0) => {}
        other => {
            zoo_rt::log!("BelugaTest did not exit cleanly: {:?}", other);
            return 18;
        }
    }
    let _ = log("Zookeeper: all checks passed");
    0
}

/// Sends `msg` and expects `pong: <msg>` back.
fn ping(h: Handle, msg: &str) -> bool {
    if send(h, msg.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    let Ok(n) = recv(h, &mut buf) else {
        return false;
    };
    let reply = &buf[..n];
    let ok = reply.len() == 6 + msg.len()
        && reply.starts_with(b"pong: ")
        && &reply[6..] == msg.as_bytes();
    zoo_rt::log!(
        "sent `{}`, got `{}`",
        msg,
        core::str::from_utf8(reply).unwrap_or("<not utf-8>")
    );
    ok
}

/// Maps `bin/zookeeper` and checks it is this ELF file, whole.
fn check_file_mapping() -> Result<(), u8> {
    let index = zoo_rt::fs_find("bin/zookeeper").ok_or(22)?;
    let size = zoo_rt::fs_stat(index).map_err(|_| 22)?.size;
    let data = zoo_rt::fs_map(index).map_err(|_| 22)?;
    if data.len() as u64 != size || !data.starts_with(b"ELF") {
        return Err(22);
    }
    zoo_rt::log!("mapped bin/zookeeper: {} bytes, ELF magic ok", data.len());
    Ok(())
}

/// Two NarwhalTests compute floating-point series at the same time; each
/// result must equal the reference computed here first, bit for bit
/// (docs/m5a-design.md §5).
fn check_narwhals() -> Result<(), u8> {
    const PARAMS: [f64; 2] = [3.25, 7.5];
    const ITERATIONS: u64 = 3_000_000;
    let mut expected = [0u64; 2];
    for (e, p) in expected.iter_mut().zip(PARAMS) {
        let mut trace = alloc::vec::Vec::new();
        *e = zoo_rt::narwhal_series(p, ITERATIONS, &mut trace).to_bits();
    }
    let mut workers = [(0u64, 0u64); 2];
    for (w, p) in workers.iter_mut().zip(PARAMS) {
        let (mine, theirs) = channel_create().map_err(|_| 23)?;
        let process = spawn("narwhal-test", Some(theirs)).map_err(|_| 23)?;
        send(mine, &p.to_bits().to_le_bytes()).map_err(|_| 23)?;
        *w = (mine, process);
    }
    for (i, &(chan, process)) in workers.iter().enumerate() {
        let mut buf = [0u8; 16];
        let n = recv(chan, &mut buf).map_err(|_| 24)?;
        let got = u64::from_le_bytes(buf[..8].try_into().map_err(|_| 24)?);
        if n != 8 || got != expected[i] {
            zoo_rt::log!(
                "NarwhalTest {} result {:#x}, expected {:#x}",
                i,
                got,
                expected[i]
            );
            return Err(24);
        }
        if wait(process) != Ok(0) {
            return Err(25);
        }
        let _ = close(chan);
        let _ = close(process);
    }
    zoo_rt::log!(
        "both NarwhalTests matched the reference bits ({:#x}, {:#x})",
        expected[0],
        expected[1]
    );
    Ok(())
}

/// Expects `call` to fail with `want`; logs and returns `code` otherwise.
fn expect_err<T: core::fmt::Debug>(
    what: &str,
    got: Result<T, Error>,
    want: Error,
    code: u8,
) -> Result<(), u8> {
    match got {
        Err(e) if e == want => Ok(()),
        other => {
            zoo_rt::log!("{}: expected {}, got {:?}", what, want.name(), other);
            Err(code)
        }
    }
}

/// Directory capabilities enforced by the kernel, then OtterHandler driven
/// through a fixed script with exact replies (docs/m5c1-design.md §5).
fn check_directories() -> Result<(), u8> {
    use zoo_rt::{dir_create, dir_open, dir_remove, dir_rights as r, dir_stat, file_read, fs_root};
    let root = fs_root().map_err(|_| 26)?;
    expect_err("second fs_root", fs_root(), Error::Rights, 26)?;
    let home = dir_open(root, "home/user", r::ALL).map_err(|_| 26)?;
    expect_err(
        "dir_open ../bin",
        dir_open(home, "../bin", r::READ),
        Error::Inval,
        26,
    )?;
    expect_err(
        "dir_open /bin",
        dir_open(home, "/bin", r::READ),
        Error::Inval,
        26,
    )?;
    expect_err(
        "dir_open a//b",
        dir_open(home, "Documents//recipes", r::READ),
        Error::Inval,
        26,
    )?;
    expect_err(
        "dir_open of a file",
        dir_open(home, "Documents/notes.txt", r::READ),
        Error::NotDir,
        26,
    )?;
    let read_only = dir_open(home, "", r::READ).map_err(|_| 26)?;
    expect_err(
        "create via read-only handle",
        dir_create(read_only, "x"),
        Error::Rights,
        26,
    )?;
    expect_err(
        "widen rights",
        dir_open(read_only, "Documents", r::ALL),
        Error::Rights,
        26,
    )?;
    expect_err(
        "create in /bin",
        dir_create(root, "bin/x"),
        Error::Rights,
        26,
    )?;
    expect_err(
        "remove non-empty",
        dir_remove(home, "Documents"),
        Error::NotEmpty,
        26,
    )?;
    expect_err(
        "remove own directory",
        dir_remove(home, ""),
        Error::Inval,
        26,
    )?;
    let mut text = [0u8; 256];
    let n = file_read(read_only, "Documents/recipes/pancakes.txt", 0, &mut text).map_err(|_| 26)?;
    if !text[..n].starts_with(b"Pancakes") {
        return Err(26);
    }
    // A handle to a removed directory sees NOTFOUND, never a reused node.
    dir_create(home, "Scratch").map_err(|_| 26)?;
    let scratch = dir_open(home, "Scratch", r::READ).map_err(|_| 26)?;
    dir_remove(home, "Scratch").map_err(|_| 26)?;
    expect_err("stale handle", dir_stat(scratch, ""), Error::NotFound, 26)?;
    // POSIX step 4: writable files (ArcticFoxTest gets home and root).
    let arcticfox_home = dir_open(home, "", r::ALL).map_err(|_| 33)?;
    let arcticfox_root = dir_open(root, "", r::ALL).map_err(|_| 33)?;
    let arcticfox = zoo_rt::spawn_with(
        "arcticfox-test",
        [Some(arcticfox_home), Some(arcticfox_root)],
    )
    .map_err(|_| 33)?;
    match wait(arcticfox) {
        Ok(0) => {
            let _ = close(arcticfox);
        }
        other => {
            zoo_rt::log!("ArcticFoxTest did not pass: {:?}", other);
            return Err(33);
        }
    }
    for h in [scratch, read_only, root] {
        let _ = close(h);
    }
    let _ = log("directory capabilities enforced (paths stay inside their handle)");

    let (mine, theirs) = channel_create().map_err(|_| 27)?;
    let otter_home = dir_open(home, "", r::ALL).map_err(|_| 27)?;
    let otter =
        zoo_rt::spawn_with("otter-handler", [Some(theirs), Some(otter_home)]).map_err(|_| 27)?;
    let script: &[(&str, &[&str])] = &[
        (
            "search pancake",
            &["| Documents/recipes/pancakes.txt", "ok"],
        ),
        (
            "search pancake recipe",
            &["| Documents/recipes/pancakes.txt", "ok"],
        ),
        ("search zebra vet", &["| Documents/notes.txt", "ok"]),
        (
            "search FLOUR",
            &[
                "| Documents/notes.txt",
                "| Documents/recipes/pancakes.txt",
                "ok",
            ],
        ),
        (
            "list",
            &[
                "| folder Documents",
                "| folder Music",
                "| folder Photos",
                "ok",
            ],
        ),
        (
            "inspect Documents",
            &["| Documents: folder, 2 entries", "ok"],
        ),
        (
            "plan Projects/2026",
            &[
                "| would create Projects",
                "| would create Projects/2026",
                "ok",
            ],
        ),
        ("inspect Projects", &["err not found"]),
        (
            "create Projects/2026",
            &["| created Projects", "| created Projects/2026", "ok"],
        ),
        ("list Projects", &["| folder 2026", "ok"]),
        (
            "undo",
            &["| removed Projects/2026", "| removed Projects", "ok"],
        ),
        ("inspect Projects", &["err not found"]),
        ("undo", &["err nothing to undo"]),
        (
            "create Documents",
            &["| exists Documents", "err already exists"],
        ),
        ("list ../bin", &["err path escapes home"]),
        ("inspect /bin", &["err path escapes home"]),
        ("delete Documents", &["err unknown tool"]),
        (
            "create Projects/2027",
            &["| created Projects", "| created Projects/2027", "ok"],
        ),
    ];
    for &(req, want) in script {
        broker_step(mine, req, want)?;
    }
    // Something added inside a created folder must survive `undo`.
    dir_create(home, "Projects/2027/Keep").map_err(|_| 27)?;
    broker_step(
        mine,
        "undo",
        &[
            "| not empty, left in place: Projects/2027",
            "| not empty, left in place: Projects",
            "ok",
        ],
    )?;
    if !dir_stat(home, "Projects/2027/Keep").is_ok_and(|e| e.is_dir()) {
        return Err(27);
    }
    for p in ["Projects/2027/Keep", "Projects/2027", "Projects"] {
        dir_remove(home, p).map_err(|_| 27)?;
    }
    send(mine, b"quit").map_err(|_| 28)?;
    if wait(otter) != Ok(0) {
        return Err(28);
    }
    for h in [mine, otter, home] {
        let _ = close(h);
    }
    zoo_rt::log!(
        "OtterHandler answered {} scripted requests exactly",
        script.len() + 1
    );
    Ok(())
}

/// Sends `req` to OtterHandler and requires exactly the reply lines `want`.
fn broker_step(chan: Handle, req: &str, want: &[&str]) -> Result<(), u8> {
    send(chan, req.as_bytes()).map_err(|_| 27)?;
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    for (k, expected) in want.iter().enumerate() {
        let n = recv(chan, &mut buf).map_err(|_| 27)?;
        let got = core::str::from_utf8(&buf[..n]).unwrap_or("<not utf-8>");
        if got != *expected {
            zoo_rt::log!(
                "broker `{}` reply {}: got `{}`, expected `{}`",
                req,
                k,
                got,
                expected
            );
            return Err(27);
        }
        let last = k + 1 == want.len();
        let done = got == "ok" || got.starts_with("err ");
        if last != done {
            zoo_rt::log!("broker `{}`: reply ended at line {}", req, k);
            return Err(27);
        }
    }
    zoo_rt::log!("broker `{}` -> {}", req, want[want.len() - 1]);
    Ok(())
}

// ---- Supervise mode (docs/s1-design.md) ---------------------------------

/// Ticks between health pings, and how long a `pong` may take.
const PING_EVERY: u64 = 100;
const PING_TIMEOUT: u64 = 200;
/// First restart back-off; doubles with each restart.
const BACKOFF: u64 = 10;

#[derive(PartialEq, Eq)]
enum SvcState {
    Running,
    /// Waiting to restart at this tick.
    Restarting(u64),
    Failed,
}

struct Svc {
    name: alloc::string::String,
    max: u32,
    state: SvcState,
    chan: Handle,
    process: Handle,
    pid: u64,
    restarts: u32,
    last: &'static str,
    /// Tick a ping was sent and not yet answered.
    ping_sent: Option<u64>,
}

struct Supervisor {
    control: Handle,
    svcs: alloc::vec::Vec<Svc>,
}

/// Reads `etc/services`: `name max_restarts` per line, `#` comments.
fn read_service_list() -> alloc::vec::Vec<(alloc::string::String, u32)> {
    let mut out = alloc::vec::Vec::new();
    let Some(data) = zoo_rt::fs_find("etc/services").and_then(|i| zoo_rt::fs_map(i).ok()) else {
        return out;
    };
    let text = core::str::from_utf8(data).unwrap_or("");
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(max)) = (parts.next(), parts.next())
            && let Ok(max) = max.parse()
        {
            out.push((alloc::string::String::from(name), max));
        }
    }
    out
}

impl Supervisor {
    /// Records a supervision event: serial log and BeaverHandler.
    fn event(&self, args: core::fmt::Arguments) {
        let mut l = zoo_rt::Line::new();
        let _ = core::fmt::Write::write_fmt(&mut l, args);
        zoo_rt::log!("Zookeeper: {}", l.as_str());
        if let Some(logger) = self
            .svcs
            .iter()
            .find(|s| s.name == "beaver-handler" && s.state == SvcState::Running)
        {
            let mut m = zoo_rt::Line::new();
            let _ = core::fmt::Write::write_fmt(&mut m, format_args!("log {}", l.as_str()));
            let _ = send(logger.chan, m.as_str().as_bytes());
        }
    }

    fn start(&mut self, k: usize) {
        let s = &mut self.svcs[k];
        let started = channel_create().and_then(|(mine, theirs)| {
            spawn(&s.name, Some(theirs))
                .map(|p| (mine, p))
                .inspect_err(|_| {
                    let _ = close(mine);
                })
        });
        match started {
            Ok((chan, process)) => {
                s.chan = chan;
                s.process = process;
                s.pid = zoo_rt::process_id(process).unwrap_or(0);
                s.state = SvcState::Running;
                s.ping_sent = None;
                let (name, pid) = (s.name.clone(), s.pid);
                self.event(format_args!("started {} (pid {})", name, pid));
            }
            Err(e) => {
                s.state = SvcState::Failed;
                s.last = "start failed";
                let name = s.name.clone();
                self.event(format_args!("cannot start {}: {}", name, e.name()));
            }
        }
    }

    /// Service `k` ended (`reason`); restart it later or give up.
    fn ended(&mut self, k: usize, reason: &'static str, now: u64) {
        let s = &mut self.svcs[k];
        let _ = close(s.chan);
        let _ = close(s.process);
        s.last = reason;
        s.ping_sent = None;
        let name = s.name.clone();
        if s.restarts < s.max {
            s.restarts += 1;
            let wait = BACKOFF << (s.restarts - 1);
            s.state = SvcState::Restarting(now + wait);
            let (n, max) = (s.restarts, s.max);
            self.event(format_args!(
                "{} {}; restarting ({} of {})",
                name, reason, n, max
            ));
        } else {
            s.state = SvcState::Failed;
            self.event(format_args!(
                "{} {}; restart limit reached, left stopped",
                name, reason
            ));
        }
    }

    fn reply(&self, text: &str) {
        let _ = zoo_rt::app::send_wait(self.control, text.as_bytes());
    }

    /// One request from the kernel shell; every reply ends with `end`.
    fn control_request(&mut self, req: &str) {
        if req == "status" {
            for s in &self.svcs {
                let state = match s.state {
                    SvcState::Running => "running",
                    SvcState::Restarting(_) => "restarting",
                    SvcState::Failed => "failed",
                };
                let mut l = zoo_rt::Line::new();
                let _ = core::fmt::Write::write_fmt(
                    &mut l,
                    format_args!(
                        "{} pid={} {} restarts={} last={}",
                        s.name, s.pid, state, s.restarts, s.last
                    ),
                );
                self.reply(l.as_str());
            }
        } else if req == "logs" {
            self.relay_logs();
        } else if let Some(rest) = req.strip_prefix("poke ") {
            let (name, what) = rest.split_once(' ').unwrap_or((rest, ""));
            match self
                .svcs
                .iter()
                .find(|s| s.name == name && s.state == SvcState::Running)
            {
                Some(s) if name == "seal-test" && matches!(what, "crash" | "hang") => {
                    let _ = send(s.chan, what.as_bytes());
                    self.reply("sent");
                }
                Some(_) => self.reply("only seal-test accepts test requests (crash, hang)"),
                None => self.reply("no such running service"),
            }
        } else {
            self.reply("unknown request");
        }
        self.reply("end");
    }

    /// Asks BeaverHandler for its lines and passes them on (bounded wait).
    fn relay_logs(&mut self) {
        let Some(k) = self
            .svcs
            .iter()
            .position(|s| s.name == "beaver-handler" && s.state == SvcState::Running)
        else {
            self.reply("beaver-handler is not running");
            return;
        };
        let chan = self.svcs[k].chan;
        if send(chan, b"dump").is_err() {
            self.reply("beaver-handler did not take the request");
            return;
        }
        let deadline = zoo_rt::ticks() + 100;
        let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
        while zoo_rt::ticks() < deadline {
            match zoo_rt::try_recv(chan, &mut buf) {
                Ok(n) if &buf[..n] == b"end" => return,
                // A late `pong` from a health ping counts as the answer.
                Ok(n) if &buf[..n] == b"pong" => self.svcs[k].ping_sent = None,
                Ok(n) => self.reply(core::str::from_utf8(&buf[..n]).unwrap_or("?")),
                Err(Error::Again) => zoo_rt::sleep(1),
                Err(_) => return,
            }
        }
        self.reply("beaver-handler did not finish in time");
    }
}

fn supervise(control: Handle) -> u8 {
    let mut sup = Supervisor {
        control,
        svcs: alloc::vec::Vec::new(),
    };
    for (name, max) in read_service_list() {
        sup.svcs.push(Svc {
            name,
            max,
            state: SvcState::Failed,
            chan: zoo_rt::NO_HANDLE,
            process: zoo_rt::NO_HANDLE,
            pid: 0,
            restarts: 0,
            last: "none",
            ping_sent: None,
        });
    }
    for k in 0..sup.svcs.len() {
        sup.start(k);
    }
    let _ = send(control, b"ready");
    let mut next_ping = zoo_rt::ticks() + PING_EVERY;
    let mut buf = [0u8; zoo_abi::MAX_MESSAGE];
    loop {
        let now = zoo_rt::ticks();
        // Requests from the kernel shell.
        match zoo_rt::try_recv(control, &mut buf) {
            Ok(n) => {
                let req =
                    alloc::string::String::from(core::str::from_utf8(&buf[..n]).unwrap_or(""));
                sup.control_request(&req);
            }
            Err(Error::Again) => {}
            Err(_) => return 0, // kernel side closed: nothing left to do
        }
        for k in 0..sup.svcs.len() {
            match sup.svcs[k].state {
                SvcState::Restarting(at) if now >= at => sup.start(k),
                SvcState::Running => {
                    let chan = sup.svcs[k].chan;
                    match zoo_rt::try_recv(chan, &mut buf) {
                        Ok(n) if &buf[..n] == b"pong" => sup.svcs[k].ping_sent = None,
                        Ok(_) | Err(Error::Again) => {}
                        Err(_) => {
                            // Its end of the channel closed: it has ended.
                            let reason = match wait(sup.svcs[k].process) {
                                Err(Error::Crashed) => "crashed",
                                Err(Error::Killed) => "killed",
                                _ => "exited",
                            };
                            sup.ended(k, reason, now);
                            continue;
                        }
                    }
                    if let Some(sent) = sup.svcs[k].ping_sent
                        && now - sent > PING_TIMEOUT
                    {
                        let _ = kill(sup.svcs[k].process);
                        let _ = wait(sup.svcs[k].process);
                        sup.ended(k, "unresponsive", now);
                    }
                }
                _ => {}
            }
        }
        if now >= next_ping {
            next_ping = now + PING_EVERY;
            for s in sup.svcs.iter_mut().filter(|s| s.state == SvcState::Running) {
                if s.ping_sent.is_none() && send(s.chan, b"ping").is_ok() {
                    s.ping_sent = Some(now);
                }
            }
        }
        zoo_rt::sleep(2);
    }
}
