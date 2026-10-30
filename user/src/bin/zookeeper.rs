//! Zookeeper: the ZooOS supervisor (docs/m3-design.md §9). In M3 it runs a
//! fixed start-up plan: start LionHandler and talk to it, supervise
//! MonkeyHandler with a one-restart policy, confirm LionHandler survived,
//! then stop everything. Each failed expectation exits with its own code.

#![no_std]
#![no_main]

extern crate alloc;

use zoo_rt::{Error, Handle, channel_create, close, kill, log, recv, send, spawn, wait};

zoo_rt::entry!(main);

/// Monkey runs once, then is restarted at most this many times.
const MONKEY_RESTARTS: u32 = 1;

/// PeacockHandler may be restarted this many times in one session.
const DESKTOP_RESTARTS: u32 = 3;

fn main(start_handle: u64) -> u8 {
    // Started with a control channel: its first message says what to do.
    if start_handle != zoo_rt::NO_HANDLE {
        let mut mode = [0u8; 32];
        return match recv(start_handle, &mut mode) {
            Ok(n) if &mode[..n] == b"session" => session(),
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

    let Ok((mine, lions)) = channel_create() else {
        return 11;
    };
    let Ok(lion) = spawn("lion-handler", Some(lions)) else {
        return 12;
    };
    if !ping(mine, "ping 1") {
        return 13;
    }

    for run in 1..=MONKEY_RESTARTS + 1 {
        let Ok(monkey) = spawn("monkey-handler", None) else {
            return 14;
        };
        match wait(monkey) {
            Err(Error::Crashed) => {
                zoo_rt::log!(
                    "MonkeyHandler crashed (run {} of {})",
                    run,
                    MONKEY_RESTARTS + 1
                )
            }
            other => {
                zoo_rt::log!("MonkeyHandler did not crash: {:?}", other);
                return 15;
            }
        }
        let _ = close(monkey);
        if run <= MONKEY_RESTARTS {
            let _ = log("restart policy allows it: restarting MonkeyHandler");
        } else {
            let _ = log("restart limit reached: giving up on MonkeyHandler");
        }
    }

    // HamsterHandler never yields. Talking to LionHandler while it runs is
    // only possible if the kernel preempts it.
    let Ok(hamster) = spawn("hamster-handler", None) else {
        return 19;
    };
    // LionHandler must have survived MonkeyHandler's crashes.
    if !ping(mine, "ping 2") {
        return 16;
    }
    if kill(hamster).is_err() {
        return 20;
    }
    match wait(hamster) {
        Err(Error::Killed) => {
            let _ = log("HamsterHandler stopped by the supervisor");
        }
        other => {
            zoo_rt::log!("HamsterHandler did not end as killed: {:?}", other);
            return 21;
        }
    }
    let _ = close(hamster);

    // M5a: file mapping, and per-process floating-point state.
    if let Err(code) = check_file_mapping() {
        return code;
    }
    if let Err(code) = check_beavers() {
        return code;
    }
    // M5c-1: directory capabilities and the action broker.
    if let Err(code) = check_directories() {
        return code;
    }
    // POSIX step 1: threads, futexes, thread-local storage.
    match spawn("ant-handler", None).map(|h| (wait(h), h)) {
        Ok((Ok(0), h)) => {
            let _ = close(h);
        }
        Ok((other, _)) => {
            zoo_rt::log!("AntHandler did not pass: {:?}", other);
            return 29;
        }
        Err(_) => return 29,
    }
    // POSIX step 2: address-space reservations and protection.
    match spawn("mole-handler", None).map(|h| (wait(h), h)) {
        Ok((Ok(0), h)) => {
            let _ = close(h);
        }
        Ok((other, _)) => {
            zoo_rt::log!("MoleHandler did not pass: {:?}", other);
            return 31;
        }
        Err(_) => return 31,
    }
    if send(mine, b"stop").is_err() {
        return 17;
    }
    match wait(lion) {
        Ok(0) => {}
        other => {
            zoo_rt::log!("LionHandler did not exit cleanly: {:?}", other);
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

/// Two BeaverHandlers compute floating-point series at the same time; each
/// result must equal the reference computed here first, bit for bit
/// (docs/m5a-design.md §5).
fn check_beavers() -> Result<(), u8> {
    const PARAMS: [f64; 2] = [3.25, 7.5];
    const ITERATIONS: u64 = 3_000_000;
    let mut expected = [0u64; 2];
    for (e, p) in expected.iter_mut().zip(PARAMS) {
        let mut trace = alloc::vec::Vec::new();
        *e = zoo_rt::beaver_series(p, ITERATIONS, &mut trace).to_bits();
    }
    let mut workers = [(0u64, 0u64); 2];
    for (w, p) in workers.iter_mut().zip(PARAMS) {
        let (mine, theirs) = channel_create().map_err(|_| 23)?;
        let process = spawn("beaver-handler", Some(theirs)).map_err(|_| 23)?;
        send(mine, &p.to_bits().to_le_bytes()).map_err(|_| 23)?;
        *w = (mine, process);
    }
    for (i, &(chan, process)) in workers.iter().enumerate() {
        let mut buf = [0u8; 16];
        let n = recv(chan, &mut buf).map_err(|_| 24)?;
        let got = u64::from_le_bytes(buf[..8].try_into().map_err(|_| 24)?);
        if n != 8 || got != expected[i] {
            zoo_rt::log!(
                "BeaverHandler {} result {:#x}, expected {:#x}",
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
        "both BeaverHandlers matched the reference bits ({:#x}, {:#x})",
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
