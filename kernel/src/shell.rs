//! In-kernel debug shell (docs/console-design.md §3). Temporary: a
//! diagnostic console until user-space programs exist, not the future shell.

use crate::memory::{self, Memory, heap};
use crate::{console, cpu, interrupts, keyboard, kprint, kprintln, serial_println};
use alloc::string::String;
use pc_keyboard::layouts::Us104Key;
use pc_keyboard::{DecodedKey, HandleControl, PS2Keyboard, ScancodeSet1};
use x86_64::instructions::interrupts as cpu_interrupts;

const PROMPT: &str = "zoo> ";
const MAX_LINE: usize = 120;

pub fn run(mut mem: Memory) -> ! {
    let drained = interrupts::enable_keyboard();
    serial_println!("ZOO_OS INFO keyboard irq=1 drained={}", drained);
    kprintln!("ZOO_OS SHELL ready");
    kprintln!("Type 'help' for commands.");
    kprint!("{}", PROMPT);

    let mut decoder = PS2Keyboard::new(ScancodeSet1::new(), Us104Key, HandleControl::Ignore);
    let mut line = String::new();
    loop {
        let byte = next_byte();
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
                execute(line.trim(), &mut mem);
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

/// Waits for the next keyboard byte, sleeping between interrupts.
fn next_byte() -> u8 {
    loop {
        // Check with interrupts off, then `sti; hlt` as one step: a key that
        // arrives after the check still wakes the CPU.
        cpu_interrupts::disable();
        if let Some(b) = keyboard::pop() {
            cpu_interrupts::enable();
            return b;
        }
        cpu_interrupts::enable_and_hlt();
    }
}

fn execute(line: &str, mem: &mut Memory) {
    let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
    match cmd {
        "" => {}
        "help" => {
            kprintln!("help            this list");
            kprintln!("echo <text>     print text");
            kprintln!("mem             physical frames and heap use");
            kprintln!("uptime          time since the timer started");
            kprintln!("keys            keyboard bytes received/dropped");
            kprintln!("clear           clear the screen");
            kprintln!("panic           deliberate kernel panic");
            kprintln!("reboot          reset the machine (QEMU exits)");
        }
        "echo" => kprintln!("{}", rest),
        "mem" => {
            let free = mem.frames.available();
            let total = mem.frames.total();
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
