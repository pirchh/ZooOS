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

zoo_rt::entry2!(main);

/// LemmingTest runs once, then is restarted at most this many times.
const LEMMING_RESTARTS: u32 = 1;

/// PeacockHandler may be restarted this many times in one session.
const DESKTOP_RESTARTS: u32 = 3;

fn main(start_handle: u64, second: u64) -> u8 {
    // Started with a control channel: its first message says what to do.
    if start_handle != zoo_rt::NO_HANDLE {
        let mut mode = [0u8; 32];
        return match recv(start_handle, &mut mode) {
            Ok(n) if &mode[..n] == b"session" => session(start_handle, second),
            Ok(n) if &mode[..n] == b"supervise" => supervise(start_handle),
            _ => 30,
        };
    }
    startup_checks()
}

/// Desktop session (docs/m4-design.md §3): run the window server, restart it
/// if it ends abnormally, finish when it exits cleanly. When the filesystem
/// service restarts, the resident supervisor sends a fresh home folder on
/// the session's line, and the desktop reloads with it (docs/s0-design.md,
/// B5): every app's folder handle came from the old one.
fn session(control: Handle, home: Handle) -> u8 {
    use zoo_rt::dir_rights as r;
    let _ = log("Zookeeper: starting the desktop session");
    // The desktop gets the user's home folder, nothing above it
    // (docs/m5c2-design.md §2): the kernel passes a folder handle the
    // resident supervisor obtained from BadgerHandler (docs/s4-design.md).
    let mut home = (home != zoo_rt::NO_HANDLE).then_some(home);
    // The supervisor's line follows the mode on the control channel.
    let mut line = match zoo_rt::try_recv_handle(control) {
        Ok((h, tag)) if tag == zoo_abi::TAG_SESSION_LINE => Some(h),
        Ok((h, _)) => {
            let _ = close(h);
            None
        }
        Err(_) => None,
    };
    let mut restarts = 0;
    loop {
        let copy = home.and_then(|h| zoo_rt::dir_open(h, "", r::ALL).ok());
        let Ok(peacock) = zoo_rt::spawn_with("peacock-handler", [None, copy]) else {
            return 2;
        };
        let mut reload = false;
        while let Some(l) = line {
            match zoo_rt::wait_any(&[peacock, l], zoo_abi::WAIT_FOREVER) {
                Ok(1) => match zoo_rt::try_recv_handle(l) {
                    Ok((fresh, _)) => {
                        if let Some(old) = home.replace(fresh) {
                            let _ = close(old);
                        }
                        let _ = log(
                            "Zookeeper: the filesystem restarted; reloading the desktop with a fresh home folder",
                        );
                        let _ = kill(peacock);
                        reload = true;
                        break;
                    }
                    Err(Error::WrongKind) => {
                        let mut skip = [0u8; zoo_abi::MAX_MESSAGE];
                        let _ = zoo_rt::try_recv(l, &mut skip);
                    }
                    Err(Error::Again) => {}
                    Err(_) => {
                        // The supervisor is gone: no more fresh folders.
                        let _ = close(l);
                        line = None;
                    }
                },
                _ => break,
            }
        }
        let outcome = wait(peacock);
        let _ = close(peacock);
        if reload {
            continue;
        }
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
    // Services step 3: handles passed in messages.
    match spawn("ptarmigan-test", None).map(|h| (wait(h), h)) {
        Ok((Ok(0), h)) => {
            let _ = close(h);
        }
        Ok((other, _)) => {
            zoo_rt::log!("PtarmiganTest did not pass: {:?}", other);
            return 34;
        }
        Err(_) => return 34,
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
    use zoo_rt::{dir_create, dir_open, dir_remove, dir_rights as r, dir_stat, file_read};
    // The filesystem is a service now (docs/s4-design.md): the kernel's old
    // file calls are gone, and the checks use a private BadgerHandler.
    let removed = zoo_rt::syscall(zoo_abi::sys::FS_ROOT, 0, 0, 0);
    if removed != Error::NoSys.as_return() {
        zoo_rt::log!("old fs_root call returned {}, expected NOSYS", removed);
        return Err(26);
    }
    let (badger, badger_control, root) = zoo_rt::start_badger().map_err(|_| 26)?;
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
            &["| Documents: folder, 3 entries", "ok"],
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
    // Stop the private BadgerHandler: it exits when its control channel
    // closes.
    let _ = close(badger_control);
    if wait(badger) != Ok(0) {
        return Err(26);
    }
    let _ = close(badger);
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
/// A service that has run this long since its last start has its restart
/// count reset (docs/s0-design.md, B5): the limit counts failures close
/// together, not every failure since boot. 30 s.
const STABLE_AFTER: u64 = 3000;

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
    /// Gets the kernel log reader as its second start-up handle.
    /// Second start-up handle: "logs" (kernel log reader) or "disk" (the
    /// data disk), from the service list.
    logs: bool,
    disk: bool,
    state: SvcState,
    chan: Handle,
    process: Handle,
    pid: u64,
    restarts: u32,
    last: &'static str,
    /// Tick a ping was sent and not yet answered.
    ping_sent: Option<u64>,
    /// Tick of its last start.
    started_at: u64,
}

struct Supervisor {
    control: Handle,
    svcs: alloc::vec::Vec<Svc>,
    /// Root folder handle from the resident BadgerHandler, when running.
    root: Option<Handle>,
    /// The channel to the current desktop session, for a fresh home
    /// folder after BadgerHandler restarts (docs/s0-design.md, B5).
    session_line: Option<Handle>,
    /// A restart the shell scheduled: (service index, tick).
    restart_at: Option<(usize, u64)>,
}

/// Reads `etc/services`: `name max_restarts [logs]` per line, `#` comments.
fn read_service_list() -> alloc::vec::Vec<(alloc::string::String, u32, bool, bool)> {
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
            let flag = parts.next();
            let (logs, disk) = (flag == Some("logs"), flag == Some("disk"));
            out.push((alloc::string::String::from(name), max, logs, disk));
        }
    }
    out
}

impl Supervisor {
    /// Records a supervision event. Like every program's log output it
    /// reaches BeaverHandler through the kernel log buffer.
    fn event(&self, args: core::fmt::Arguments) {
        let mut l = zoo_rt::Line::new();
        let _ = core::fmt::Write::write_fmt(&mut l, args);
        zoo_rt::log!("Zookeeper: {}", l.as_str());
    }

    fn start(&mut self, k: usize) {
        let s = &mut self.svcs[k];
        // A fresh log reader for each start: handles move into the child.
        let reader = if s.logs {
            zoo_rt::log_attach().ok()
        } else if s.disk {
            zoo_rt::block_attach().ok()
        } else {
            None
        };
        let started = channel_create().and_then(|(mine, theirs)| {
            zoo_rt::spawn_with(&s.name, [Some(theirs), reader])
                .map(|p| (mine, p))
                .inspect_err(|_| {
                    let _ = close(mine);
                    if let Some(r) = reader {
                        let _ = close(r);
                    }
                })
        });
        match started {
            Ok((chan, process)) => {
                s.chan = chan;
                s.process = process;
                s.pid = zoo_rt::process_id(process).unwrap_or(0);
                s.state = SvcState::Running;
                s.ping_sent = None;
                s.started_at = zoo_rt::ticks();
                if s.name == "badger-handler" {
                    // Ask the new filesystem service for its root folder.
                    let _ = send(s.chan, b"root");
                }
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

    /// Sends the desktop session, if one is running, a home folder from the
    /// current root: its old one died with the previous BadgerHandler.
    fn refresh_session(&mut self) {
        let Some(line) = self.session_line else {
            return;
        };
        let Some(home) = self
            .root
            .and_then(|r| zoo_rt::dir_open(r, "home/user", zoo_rt::dir_rights::ALL).ok())
        else {
            return;
        };
        if zoo_rt::send_handle(line, home, zoo_abi::TAG_SESSION_LINE).is_ok() {
            self.event(format_args!("sent the desktop session a fresh home folder"));
        } else {
            // The session has ended.
            let _ = close(home);
            let _ = close(line);
            self.session_line = None;
        }
    }

    /// Service `k` ended (`reason`); restart it later or give up.
    fn ended(&mut self, k: usize, reason: &'static str, now: u64) {
        if self.svcs[k].name == "badger-handler"
            && let Some(old) = self.root.take()
        {
            let _ = close(old);
        }
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

    /// `connect <target>` from service `k` (docs/s3-design.md §2): a fresh
    /// channel, one end to the target (tag 1), one to the requester (tag 2).
    fn connect(&mut self, k: usize, target: &str) {
        let requester = self.svcs[k].chan;
        let Some(t) = self
            .svcs
            .iter()
            .position(|s| s.name == target && s.state == SvcState::Running)
        else {
            let _ = send(requester, b"err no such running service");
            return;
        };
        let Ok((for_target, for_requester)) = channel_create() else {
            let _ = send(requester, b"err out of handles");
            return;
        };
        if zoo_rt::send_handle(self.svcs[t].chan, for_target, 1).is_err() {
            let _ = close(for_target);
            let _ = close(for_requester);
            let _ = send(requester, b"err target did not take the connection");
            return;
        }
        if zoo_rt::send_handle(requester, for_requester, 2).is_err() {
            let _ = close(for_requester);
        }
        let name = self.svcs[k].name.clone();
        self.event(format_args!("connected {} to {}", name, target));
    }

    /// `file dir|cat|put ...` from the kernel shell: simple access to the
    /// home folder through BadgerHandler (docs/s5-design.md).
    fn file_request(&self, req: &str) {
        use zoo_rt::{
            dir_list, dir_open, dir_rights, file_create, file_read, file_truncate, file_write,
        };
        let Some(home) = self
            .root
            .and_then(|r| dir_open(r, "home/user", dir_rights::ALL).ok())
        else {
            self.reply("no filesystem service");
            return;
        };
        let (cmd, rest) = req.split_once(' ').unwrap_or((req, ""));
        match cmd {
            "dir" => {
                let path = rest.trim();
                for i in 0.. {
                    match dir_list(home, path, i) {
                        Ok(e) => {
                            let mut l = zoo_rt::Line::new();
                            let _ = if e.is_dir() {
                                core::fmt::Write::write_fmt(&mut l, format_args!("  {}/", e.name()))
                            } else {
                                core::fmt::Write::write_fmt(
                                    &mut l,
                                    format_args!("  {} ({} bytes)", e.name(), e.size),
                                )
                            };
                            self.reply(l.as_str());
                        }
                        Err(Error::NotFound) if i > 0 => break,
                        Err(e) => {
                            self.reply(e.name());
                            break;
                        }
                    }
                }
            }
            "cat" => {
                let mut buf = [0u8; 4096];
                match file_read(home, rest.trim(), 0, &mut buf) {
                    Ok(n) => {
                        let text = core::str::from_utf8(&buf[..n]).unwrap_or("<not text>");
                        for line in text.lines() {
                            self.reply(line);
                        }
                    }
                    Err(e) => self.reply(e.name()),
                }
            }
            "put" => {
                let (path, text) = rest.split_once(' ').unwrap_or((rest, ""));
                let _ = file_create(home, path);
                let r = file_truncate(home, path, 0).and_then(|_| {
                    // `\t` writes a tab (the shell cannot type one).
                    let text = text.replace("\\t", "\t");
                    let mut data = alloc::vec::Vec::from(text.as_bytes());
                    data.push(b'\n');
                    file_write(home, path, 0, &data)
                });
                match r {
                    Ok(_) => self.reply("saved"),
                    Err(e) => self.reply(e.name()),
                }
            }
            _ => self.reply("file: dir [path] | cat <path> | put <path> <text>"),
        }
        let _ = close(home);
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
        } else if let Some(rest) = req.strip_prefix("restart ") {
            // Kill a running service; its restart policy brings it back.
            // `restart <svc> after <seconds>` does it later, so a test can
            // restart a service while the desktop has the keyboard.
            let (name, after) = match rest.split_once(" after ") {
                Some((n, secs)) => (n, secs.trim().parse::<u64>().ok()),
                None => (rest, None),
            };
            match self
                .svcs
                .iter()
                .position(|s| s.name == name && s.state == SvcState::Running)
            {
                Some(k) => match after {
                    Some(secs) => {
                        self.restart_at = Some((k, zoo_rt::ticks() + secs * 100));
                        self.reply("scheduled");
                    }
                    None => {
                        let _ = kill(self.svcs[k].process);
                        self.reply("killed; the restart policy applies");
                    }
                },
                None => self.reply("no such running service"),
            }
        } else if let Some(rest) = req.strip_prefix("file ") {
            self.file_request(rest);
        } else if req == "home" {
            // A home-folder handle for the next desktop session, after a
            // line to that session (the kernel keeps both until it starts).
            if let Ok((mine, theirs)) = channel_create() {
                if zoo_rt::send_handle(self.control, theirs, zoo_abi::TAG_SESSION_LINE).is_ok() {
                    if let Some(old) = self.session_line.replace(mine) {
                        let _ = close(old);
                    }
                } else {
                    let _ = close(mine);
                    let _ = close(theirs);
                }
            }
            match self
                .root
                .and_then(|r| zoo_rt::dir_open(r, "home/user", zoo_rt::dir_rights::ALL).ok())
            {
                Some(h) => {
                    if zoo_rt::send_handle(self.control, h, 0).is_err() {
                        let _ = close(h);
                    }
                }
                None => self.reply("no filesystem service"),
            }
        } else if let Some(filter) = req.strip_prefix("logs") {
            let filter = alloc::string::String::from(filter.trim());
            self.relay_logs(&filter);
        } else if let Some(rest) = req.strip_prefix("poke ") {
            let (name, what) = rest.split_once(' ').unwrap_or((rest, ""));
            match self
                .svcs
                .iter()
                .find(|s| s.name == name && s.state == SvcState::Running)
            {
                Some(s)
                    if (name == "seal-test" && matches!(what, "crash" | "hang" | "connect"))
                        || (name == "badger-handler" && what == "half-save") =>
                {
                    let _ = send(s.chan, what.as_bytes());
                    self.reply("sent");
                }
                Some(_) => {
                    self.reply("only seal-test accepts test requests (crash, hang, connect)")
                }
                None => self.reply("no such running service"),
            }
        } else {
            self.reply("unknown request");
        }
        self.reply("end");
    }

    /// Asks BeaverHandler for its lines and passes them on (bounded wait).
    fn relay_logs(&mut self, filter: &str) {
        let Some(k) = self
            .svcs
            .iter()
            .position(|s| s.name == "beaver-handler" && s.state == SvcState::Running)
        else {
            self.reply("beaver-handler is not running");
            return;
        };
        let chan = self.svcs[k].chan;
        let mut req = zoo_rt::Line::new();
        let _ = core::fmt::Write::write_fmt(&mut req, format_args!("dump {}", filter));
        if send(chan, req.as_str().as_bytes()).is_err() {
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

/// Longest wait for the filesystem root before reporting ready (ticks).
const READY_WAIT: u64 = 300;

fn supervise(control: Handle) -> u8 {
    let mut sup = Supervisor {
        control,
        svcs: alloc::vec::Vec::new(),
        root: None,
        session_line: None,
        restart_at: None,
    };
    for (name, max, logs, disk) in read_service_list() {
        sup.svcs.push(Svc {
            name,
            max,
            logs,
            disk,
            state: SvcState::Failed,
            chan: zoo_rt::NO_HANDLE,
            process: zoo_rt::NO_HANDLE,
            pid: 0,
            restarts: 0,
            last: "none",
            ping_sent: None,
            started_at: 0,
        });
    }
    for k in 0..sup.svcs.len() {
        sup.start(k);
    }
    // `ready` waits for the filesystem service's home folder, so the shell
    // prompt comes after its start-up (it loads the disk first); bounded,
    // so a failing service cannot hold up the boot.
    let wants_root = sup.svcs.iter().any(|s| s.name == "badger-handler");
    let ready_by = zoo_rt::ticks() + READY_WAIT;
    let mut ready_sent = false;
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
        if let Some((k, at)) = sup.restart_at
            && now >= at
        {
            sup.restart_at = None;
            if sup.svcs[k].state == SvcState::Running {
                let name = sup.svcs[k].name.clone();
                sup.event(format_args!("restarting {} as scheduled", name));
                let _ = kill(sup.svcs[k].process);
            }
        }
        for k in 0..sup.svcs.len() {
            match sup.svcs[k].state {
                SvcState::Restarting(at) if now >= at => sup.start(k),
                SvcState::Running => {
                    let s = &mut sup.svcs[k];
                    if s.restarts > 0 && now.saturating_sub(s.started_at) >= STABLE_AFTER {
                        s.restarts = 0;
                        let name = s.name.clone();
                        sup.event(format_args!(
                            "{} ran 30 s without failing; restart count reset",
                            name
                        ));
                    }
                    let chan = sup.svcs[k].chan;
                    match zoo_rt::try_recv(chan, &mut buf) {
                        Ok(n) if &buf[..n] == b"pong" => sup.svcs[k].ping_sent = None,
                        Ok(n) if buf[..n].starts_with(b"connect ") => {
                            let target = alloc::string::String::from(
                                core::str::from_utf8(&buf[8..n]).unwrap_or(""),
                            );
                            sup.connect(k, &target);
                        }
                        // Services send Zookeeper no handles; drop any.
                        Err(Error::WrongKind) => {
                            if let Ok((h, tag)) = zoo_rt::try_recv_handle(chan) {
                                if sup.svcs[k].name == "badger-handler"
                                    && tag == zoo_rt::fsproto::TAG_ROOT
                                {
                                    if let Some(old) = sup.root.replace(h) {
                                        let _ = close(old);
                                    }
                                    let pid = sup.svcs[k].pid;
                                    sup.event(format_args!(
                                        "filesystem root from badger-handler (pid {})",
                                        pid
                                    ));
                                    sup.refresh_session();
                                } else {
                                    let _ = close(h);
                                }
                            }
                        }
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
        // Sleep until a message arrives on any channel we serve, or the
        // next ping, timeout check or restart is due (docs/s4a-design.md).
        let mut watch = alloc::vec![control];
        let mut due = next_ping;
        for s in &sup.svcs {
            match s.state {
                SvcState::Running => {
                    if watch.len() < zoo_abi::WAIT_ANY_MAX {
                        watch.push(s.chan);
                    }
                    if let Some(sent) = s.ping_sent {
                        due = due.min(sent + PING_TIMEOUT + 1);
                    }
                }
                SvcState::Restarting(at) => due = due.min(at),
                SvcState::Failed => {}
            }
        }
        let now = zoo_rt::ticks();
        // Checked after this round's messages (the root may just have
        // arrived), so `ready` goes out without waiting for a later wake.
        if !ready_sent {
            if !wants_root || sup.root.is_some() || now >= ready_by {
                if sup.root.is_none() && wants_root {
                    sup.event(format_args!("ready without a filesystem root"));
                }
                let _ = send(control, b"ready");
                ready_sent = true;
            } else {
                due = due.min(ready_by);
            }
        }
        let _ = zoo_rt::wait_any(&watch, due.saturating_sub(now).max(1));
    }
}
