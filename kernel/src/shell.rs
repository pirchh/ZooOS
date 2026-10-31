//! In-kernel debug shell (docs/console-design.md §3). Temporary: a
//! diagnostic console until user-space programs exist, not the future shell.

use crate::memory::{self, heap};
use crate::{
    console, cpu, initrd, interrupts, keyboard, kprint, kprintln, process, serial_println,
};
use alloc::string::String;
use pc_keyboard::layouts::Us104Key;
use pc_keyboard::{DecodedKey, HandleControl, PS2Keyboard, ScancodeSet1};
use x86_64::instructions::interrupts as cpu_interrupts;

const PROMPT: &str = "zoo> ";
const MAX_LINE: usize = 120;

pub fn run() -> ! {
    let drained = interrupts::enable_keyboard();
    serial_println!("ZOO_OS INFO keyboard irq=1 drained={}", drained);
    kprintln!("ZOO_OS SHELL ready");
    kprintln!("Type 'help' for commands.");
    kprint!("{}", PROMPT);

    let mut decoder = PS2Keyboard::new(ScancodeSet1::new(), Us104Key, HandleControl::Ignore);
    let mut line = String::new();
    loop {
        let Some(byte) = next_byte() else {
            // A desktop session gave the screen back.
            line.clear();
            kprintln!("Desktop session ended.");
            kprint!("{}", PROMPT);
            continue;
        };
        let Ok(Some(event)) = decoder.add_byte(byte) else {
            continue;
        };
        let Some(DecodedKey::Unicode(c)) = decoder.process_keyevent(event) else {
            continue;
        };
        match c {
            '\n' => {
                kprint!("\n");
                serial_println!("ZOO_OS SHELL line={}", line);
                execute(line.trim());
                line.clear();
                kprint!("{}", PROMPT);
            }
            '\u{8}' => {
                if line.pop().is_some() {
                    console::with(|c| c.backspace());
                    crate::serial::_print(format_args!("\u{8} \u{8}"));
                }
            }
            c if (' '..='~').contains(&c) && line.len() < MAX_LINE => {
                line.push(c);
                kprint!("{}", c);
            }
            _ => {}
        }
    }
}

/// Waits for the next keyboard byte, sleeping between interrupts. Returns
/// `None` when a desktop session has just released the display. While a
/// process owns the display, the keyboard belongs to it and is left alone.
fn next_byte() -> Option<u8> {
    let mut was_owned = false;
    loop {
        // Check with interrupts off, then `sti; hlt` as one step: a key that
        // arrives after the check still wakes the CPU.
        cpu_interrupts::disable();
        let owned = process::display_owned();
        if was_owned && !owned {
            cpu_interrupts::enable();
            // The window server let go of the screen. If its session is
            // restarting it, it will take the screen back; otherwise wait
            // (bounded) for the session to finish, so its last output comes
            // before our prompt rather than after it.
            if wait_for_session_to_settle() {
                continue;
            }
            return None;
        }
        was_owned = owned;
        if !owned && let Some(b) = keyboard::pop() {
            cpu_interrupts::enable();
            return Some(b);
        }
        // Idle point: user processes may run while the shell waits.
        process::idle_hlt();
        cpu_interrupts::enable();
    }
}

/// After the display is released: true if a process took it back (keep
/// waiting as before), false once the desktop session has ended or 5 s pass.
fn wait_for_session_to_settle() -> bool {
    let limit = interrupts::ticks() + 5 * u64::from(interrupts::TIMER_HZ);
    while interrupts::ticks() < limit {
        if process::display_owned() {
            return true;
        }
        if !process::session_running() {
            wait_for_session_processes();
            return false;
        }
        cpu_interrupts::disable();
        process::idle_hlt();
        cpu_interrupts::enable();
    }
    false
}

/// User processes alive before the desktop started (the resident
/// supervisor and its services).
static TASKS_BEFORE_SESSION: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// After the session's Zookeeper ends, its apps may still be exiting. Wait
/// (bounded, 2 s) until only the processes from before the session remain,
/// so their exit lines come before our prompt (docs/s4a-design.md §4).
fn wait_for_session_processes() {
    let before = TASKS_BEFORE_SESSION.load(core::sync::atomic::Ordering::Relaxed);
    let limit = interrupts::ticks() + 2 * u64::from(interrupts::TIMER_HZ);
    while interrupts::ticks() < limit && process::user_tasks() > before {
        cpu_interrupts::disable();
        process::idle_hlt();
        cpu_interrupts::enable();
    }
}

/// Sends `req` to the resident supervisor and prints its reply lines until
/// `end` (bounded: 3 s).
fn supervisor_request(req: &str) {
    // Drop anything left over from an earlier request that timed out.
    while process::supervisor_try_recv().is_some() {}
    if let Err(e) = process::supervisor_send(req.as_bytes()) {
        kprintln!("supervisor: {}", e.name());
        return;
    }
    let limit = interrupts::ticks() + 3 * u64::from(interrupts::TIMER_HZ);
    while interrupts::ticks() < limit {
        match process::supervisor_try_recv() {
            Some(m) if m == b"end" => return,
            Some(m) => kprintln!("{}", core::str::from_utf8(&m).unwrap_or("<not utf-8>")),
            None => {
                cpu_interrupts::disable();
                process::idle_hlt();
                cpu_interrupts::enable();
            }
        }
    }
    kprintln!("supervisor did not answer within 3 s");
}

/// Asks the resident supervisor for a home-folder handle for the desktop
/// session (bounded, 3 s). Without one the desktop still starts, without
/// file access.
fn ask_for_home() {
    while process::supervisor_try_recv().is_some() {}
    if process::supervisor_send(b"home").is_err() {
        return;
    }
    let limit = interrupts::ticks() + 3 * u64::from(interrupts::TIMER_HZ);
    while interrupts::ticks() < limit {
        match process::supervisor_try_recv() {
            Some(m) if m == b"<handle>" => return,
            Some(_) => {}
            None => {
                cpu_interrupts::disable();
                process::idle_hlt();
                cpu_interrupts::enable();
            }
        }
    }
    kprintln!("no home folder from the supervisor; the desktop will have no files");
}

fn execute(line: &str) {
    let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
    match cmd {
        "" => {}
        "help" => {
            kprintln!("help            this list");
            kprintln!("echo <text>     print text");
            kprintln!("mem             physical frames and heap use");
            kprintln!("uptime          time since the timer started");
            kprintln!("keys            keyboard bytes received/dropped");
            kprintln!("ps              list processes");
            kprintln!("ls              list the initial filesystem");
            kprintln!("run <program>   start a program from /bin");
            kprintln!("desktop         start the desktop (Esc in it returns here)");
            kprintln!("services        system services and their state");
            kprintln!("logs [text]     recent log lines, optionally only those containing text");
            kprintln!("poke <svc> <x>  send a test request to a service");
            kprintln!("restart <svc>   stop a service; its restart policy starts it again");
            kprintln!("clear           clear the screen");
            kprintln!("panic           deliberate kernel panic");
            kprintln!("reboot          reset the machine (QEMU exits)");
        }
        "echo" => kprintln!("{}", rest),
        "mem" => {
            let (free, total) = memory::with(|m| (m.frames.available(), m.frames.total()));
            kprintln!("frames: {} free of {} ({} KiB free)", free, total, free * 4);
            kprintln!(
                "heap:   {} bytes used of {}",
                heap::used(),
                memory::HEAP_SIZE
            );
        }
        "uptime" => {
            let t = interrupts::ticks();
            let hz = u64::from(interrupts::TIMER_HZ);
            kprintln!("{} ticks, {}.{:02} s", t, t / hz, (t % hz) * 100 / hz);
        }
        "keys" => {
            let (received, dropped) = keyboard::stats();
            kprintln!("received {} bytes, dropped {}", received, dropped);
        }
        "ps" => process::for_each_task(|pid, name, state| {
            kprintln!("{:>4}  {:<16} {}", pid, name, state);
        }),
        "ls" => initrd::for_each(|name, size| kprintln!("{:>8}  {}", size, name)),
        "run" => match process::spawn_from_kernel(rest.trim()) {
            Ok(pid) => kprintln!("started pid {}", pid),
            Err(e) => kprintln!("run {}: {}", rest.trim(), e.name()),
        },
        "desktop" => {
            TASKS_BEFORE_SESSION
                .store(process::user_tasks(), core::sync::atomic::Ordering::Relaxed);
            ask_for_home();
            match process::start_session() {
                Ok(pid) => kprintln!("starting the desktop (zookeeper pid {})", pid),
                Err(e) => kprintln!("desktop: {}", e.name()),
            }
        }
        "services" => supervisor_request("status"),
        "restart" => {
            let mut req = String::from("restart ");
            req.push_str(rest.trim());
            supervisor_request(&req);
        }
        "logs" => {
            let mut req = String::from("logs ");
            req.push_str(rest.trim());
            supervisor_request(&req);
        }
        "poke" => {
            let mut req = String::from("poke ");
            req.push_str(rest.trim());
            supervisor_request(&req);
        }
        "clear" => {
            console::with(|c| c.clear());
        }
        "panic" => panic!("requested from shell"),
        "reboot" => {
            kprintln!("rebooting");
            keyboard::request_reset();
            kprintln!("reset request had no effect; halting");
            cpu::halt_forever();
        }
        other => kprintln!("unknown command: {} (try 'help')", other),
    }
}
