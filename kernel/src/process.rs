//! Processes, scheduling, handles, channels and system calls
//! (docs/m3-design.md §3–§6; display, input, sleep and files added by
//! docs/m4-design.md §2).
//!
//! All state lives in `SCHED`, locked only with interrupts disabled: from the
//! trap dispatcher (interrupts are off there) or from kernel-task code inside
//! `without_interrupts`. Lock order: `SCHED`, then `memory::with`.

use crate::console;
use crate::gdt;
use crate::initrd;
use crate::interrupts;
use crate::keyboard;
use crate::kprintln;
use crate::memory::{
    self, KSTACK_SLOTS, Memory, USER_IMAGE_END, USER_IMAGE_START, USER_STACK_SIZE, USER_STACK_TOP,
};
use crate::trap::{self, TrapFrame};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bootloader_api::info::PixelFormat;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts as cpu_interrupts;
use x86_64::registers::control::{Cr2, Cr3};
use x86_64::structures::paging::{PhysFrame, Translate};
use zoo_abi::{
    ABI_VERSION, CHANNEL_DEPTH, DISPLAY_BASE, DisplayInfo, Error, FORMAT_BGR, FORMAT_RGB, FsEntry,
    MAX_FS_NAME, MAX_HANDLES, MAX_MESSAGE, MAX_PATH, NO_HANDLE, elf, sys,
};

/// Kernel task plus one task per kernel-stack slot.
const MAX_TASKS: usize = 1 + KSTACK_SLOTS;
const RIGHT_SEND: u8 = 1;
const RIGHT_RECV: u8 = 2;
const RIGHT_WAIT: u8 = 4;
const RIGHT_KILL: u8 = 8;
const RFLAGS_IF: u64 = 1 << 9;
const RFLAGS_RESERVED: u64 = 1 << 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Blocked,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Code(u8),
    Crashed,
    Killed,
}

struct Channel {
    /// `queues[s]` holds messages waiting to be received on side `s`.
    queues: [VecDeque<Vec<u8>>; 2],
    open: [bool; 2],
}

#[derive(Clone)]
enum Object {
    Endpoint {
        chan: Arc<Mutex<Channel>>,
        side: usize,
    },
    Process {
        pid: u64,
    },
}

#[derive(Clone)]
struct HandleEntry {
    object: Object,
    rights: u8,
}

/// Releases what a handle held. Channel ends close (the peer sees
/// `PEERCLOSED`); process handles hold nothing.
fn release(entry: HandleEntry) {
    if let Object::Endpoint { chan, side } = entry.object {
        let mut c = chan.lock();
        c.open[side] = false;
        c.queues[side].clear();
    }
}

struct Task {
    pid: u64,
    name: String,
    state: State,
    /// Saved `*mut TrapFrame` (as an address), valid while not running.
    frame: u64,
    /// Re-run the system call in `frame` when next scheduled.
    restart: bool,
    /// `None` for the kernel task.
    p4: Option<PhysFrame>,
    /// (slot, stack top); `None` for the kernel task.
    kstack: Option<(usize, u64)>,
    handles: [Option<HandleEntry>; MAX_HANDLES],
    generation: [u16; MAX_HANDLES],
    /// Deadline (in ticks) of an unfinished `sleep`.
    sleep_until: Option<u64>,
    /// Framebuffer pages mapped at `DISPLAY_BASE` (0 if none).
    display_pages: u64,
}

impl Task {
    fn new(pid: u64, name: &str) -> Task {
        Task {
            pid,
            name: String::from(name),
            state: State::Ready,
            frame: 0,
            restart: false,
            p4: None,
            kstack: None,
            handles: [const { None }; MAX_HANDLES],
            generation: [0; MAX_HANDLES],
            sleep_until: None,
            display_pages: 0,
        }
    }

    fn free_slots(&self) -> usize {
        self.handles.iter().filter(|h| h.is_none()).count()
    }

    /// Stores `entry`; returns its handle value `generation << 8 | slot+1`.
    fn insert(&mut self, entry: HandleEntry) -> Option<u64> {
        let i = self.handles.iter().position(|h| h.is_none())?;
        self.handles[i] = Some(entry);
        Some(((self.generation[i] as u64) << 8) | (i as u64 + 1))
    }

    fn slot_of(&self, value: u64) -> Option<usize> {
        let slot = (value & 0xFF) as usize;
        if slot == 0 || slot > MAX_HANDLES {
            return None;
        }
        let i = slot - 1;
        if value >> 8 != self.generation[i] as u64 || self.handles[i].is_none() {
            return None;
        }
        Some(i)
    }

    fn get(&self, value: u64) -> Option<&HandleEntry> {
        self.slot_of(value).and_then(|i| self.handles[i].as_ref())
    }

    /// Removes a handle; its slot's generation changes, so the old value
    /// becomes stale.
    fn take(&mut self, value: u64) -> Option<HandleEntry> {
        let i = self.slot_of(value)?;
        self.generation[i] = self.generation[i].wrapping_add(1);
        self.handles[i].take()
    }
}

enum Outcome {
    Ret(i64),
    Block,
    Yield,
    Exit(u8),
}

fn err(e: Error) -> Outcome {
    Outcome::Ret(e.as_return())
}

struct Scheduler {
    tasks: Vec<Option<Task>>,
    current: usize,
    next_pid: u64,
    exits: Vec<(u64, ExitStatus)>,
    zombies: Vec<usize>,
    kstack_used: [bool; KSTACK_SLOTS],
    /// Pid of the process that owns the display and keyboard.
    display_owner: Option<u64>,
    /// The kernel's end of the current desktop session's control channel,
    /// with that session's Zookeeper pid.
    control: Option<(u64, HandleEntry)>,
}

/// Mirrors `display_owner.is_some()` for the shell's keyboard loop.
static DISPLAY_OWNED: AtomicBool = AtomicBool::new(false);

/// True while a user process owns the display and keyboard.
pub fn display_owned() -> bool {
    DISPLAY_OWNED.load(Ordering::Relaxed)
}

static SCHED: Mutex<Option<Scheduler>> = Mutex::new(None);

/// Set by the kernel task right before `sti; hlt`: the only point where it
/// may be preempted (docs/m3-design.md §3).
static KERNEL_IDLE: AtomicBool = AtomicBool::new(false);

fn with_sched<R>(f: impl FnOnce(&mut Scheduler, &mut Memory) -> R) -> R {
    cpu_interrupts::without_interrupts(|| {
        let mut g = SCHED
            .try_lock()
            .expect("scheduler lock already held (re-entrant use)");
        let s = g.as_mut().expect("scheduler not initialised");
        memory::with(|m| f(s, m))
    })
}

pub fn init() {
    cpu_interrupts::without_interrupts(|| {
        let mut tasks = Vec::with_capacity(MAX_TASKS);
        tasks.push(Some(Task::new(0, "kernel")));
        *SCHED.lock() = Some(Scheduler {
            tasks,
            current: 0,
            next_pid: 1,
            exits: Vec::new(),
            zombies: Vec::new(),
            kstack_used: [false; KSTACK_SLOTS],
            display_owner: None,
            control: None,
        });
    });
}

/// Halts until the next interrupt, allowing a switch to a user task. Call
/// with interrupts disabled; returns with interrupts disabled.
pub fn idle_hlt() {
    KERNEL_IDLE.store(true, Ordering::Relaxed);
    cpu_interrupts::enable_and_hlt();
    cpu_interrupts::disable();
    KERNEL_IDLE.store(false, Ordering::Relaxed);
}

/// Number of user tasks not yet reaped.
pub fn user_tasks() -> usize {
    with_sched(|s, _| s.tasks.iter().skip(1).filter(|t| t.is_some()).count())
}

/// Starts `name` from the kernel task (no parent handle). Returns its pid.
pub fn spawn_from_kernel(name: &str) -> Result<u64, Error> {
    with_sched(|s, m| s.spawn(m, name, None).map(|(_, pid, _)| pid))
}

/// One line per task, for the shell's `ps`.
pub fn for_each_task(mut f: impl FnMut(u64, &str, &str)) {
    with_sched(|s, _| {
        for t in s.tasks.iter().flatten() {
            let state = match t.state {
                State::Ready => "ready",
                State::Blocked => "blocked",
                State::Exited => "exited",
            };
            f(t.pid, &t.name, state);
        }
    })
}

/// True while the current desktop session's Zookeeper has not exited.
pub fn session_running() -> bool {
    with_sched(|s, _| match &s.control {
        Some((pid, _)) => s
            .tasks
            .iter()
            .flatten()
            .any(|t| t.pid == *pid && t.state != State::Exited),
        None => false,
    })
}

/// Starts a desktop session: Zookeeper in session mode, with a control
/// channel whose first message is `session` (docs/m4-design.md §2).
pub fn start_session() -> Result<u64, Error> {
    with_sched(|s, m| {
        if let Some((pid, _)) = &s.control
            && s.tasks
                .iter()
                .flatten()
                .any(|t| t.pid == *pid && t.state != State::Exited)
        {
            return Err(Error::Busy);
        }
        let chan = Arc::new(Mutex::new(Channel {
            queues: [VecDeque::new(), VecDeque::new()],
            open: [true, true],
        }));
        // Side 1 is Zookeeper's: its receive queue holds the mode.
        chan.lock().queues[1].push_back(b"session".to_vec());
        let rights = RIGHT_SEND | RIGHT_RECV;
        let kernel_end = HandleEntry {
            object: Object::Endpoint {
                chan: chan.clone(),
                side: 0,
            },
            rights,
        };
        let zookeeper_end = HandleEntry {
            object: Object::Endpoint { chan, side: 1 },
            rights,
        };
        let (_, pid, _) = s.spawn(m, "zookeeper", Some(zookeeper_end))?;
        if let Some((_, old)) = s.control.replace((pid, kernel_end)) {
            release(old);
        }
        Ok(pid)
    })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

impl Scheduler {
    fn task(&mut self, i: usize) -> &mut Task {
        self.tasks[i].as_mut().expect("task slot in use")
    }

    fn wake_all(&mut self) {
        for t in self.tasks.iter_mut().flatten() {
            if t.state == State::Blocked {
                t.state = State::Ready;
            }
        }
    }

    /// Next ready task after the current one (round-robin). The kernel task
    /// is always ready, so this always finds one.
    fn pick_next(&self) -> usize {
        let n = self.tasks.len();
        (1..=n)
            .map(|k| (self.current + k) % n)
            .find(|&i| matches!(&self.tasks[i], Some(t) if t.state == State::Ready))
            .unwrap_or(0)
    }

    /// Loads, maps and readies program `name`; `give` becomes its start-up
    /// handle. Returns (task index, pid, start-up handle value).
    fn spawn(
        &mut self,
        mem: &mut Memory,
        name: &str,
        give: Option<HandleEntry>,
    ) -> Result<(usize, u64, u64), Error> {
        if !valid_name(name) || name.len() > MAX_PATH {
            return Err(Error::Inval);
        }
        let mut path = String::from("bin/");
        path.push_str(name);
        let file = initrd::find(&path).ok_or(Error::NotFound)?;
        let image = elf::parse(file, USER_IMAGE_START, USER_IMAGE_END).map_err(|e| {
            kprintln!("ZOO_OS INFO spawn {} rejected: {:?}", name, e);
            Error::Inval
        })?;
        let index = match self.tasks.iter().position(|t| t.is_none()) {
            Some(i) => i,
            None if self.tasks.len() < MAX_TASKS => {
                self.tasks.push(None);
                self.tasks.len() - 1
            }
            None => return Err(Error::NoMem),
        };
        let slot = self
            .kstack_used
            .iter()
            .position(|u| !u)
            .ok_or(Error::NoMem)?;

        let p4 = mem.new_address_space().map_err(|_| Error::NoMem)?;
        if load_image(mem, p4, file, &image).is_err() {
            mem.free_address_space(p4);
            return Err(Error::NoMem);
        }
        let Ok(top) = mem.map_kernel_stack(slot) else {
            mem.free_address_space(p4);
            return Err(Error::NoMem);
        };
        self.kstack_used[slot] = true;

        let pid = self.next_pid;
        self.next_pid += 1;
        let mut task = Task::new(pid, name);
        let arg = match give {
            Some(entry) => task.insert(entry).expect("new task has free handle slots"),
            None => NO_HANDLE,
        };
        let sel = gdt::selectors();
        let frame = TrapFrame {
            rip: image.entry,
            cs: sel.user_code.0 as u64,
            rflags: RFLAGS_IF | RFLAGS_RESERVED,
            // SysV: at function entry, rsp + 8 is 16-byte aligned.
            rsp: USER_STACK_TOP - 8,
            ss: sel.user_data.0 as u64,
            rdi: arg,
            ..TrapFrame::default()
        };
        let frame_addr = top - core::mem::size_of::<TrapFrame>() as u64;
        // SAFETY: the kernel stack `[top - KSTACK_SIZE, top)` was just mapped
        // writable and belongs to this task alone; the frame fits at its top.
        unsafe { (frame_addr as *mut TrapFrame).write(frame) };
        task.frame = frame_addr;
        task.p4 = Some(p4);
        task.kstack = Some((slot, top));
        self.tasks[index] = Some(task);
        kprintln!("ZOO_OS PROC start pid={} name={}", pid, name);
        Ok((index, pid, arg))
    }

    /// Ends task `i`: closes its handles, records the status, wakes waiters.
    /// Its memory is reclaimed later by `reap`, off its own stack.
    fn finish(&mut self, i: usize, status: ExitStatus) {
        let t = self.task(i);
        t.state = State::Exited;
        t.restart = false;
        let (pid, name) = (t.pid, t.name.clone());
        let handles: Vec<HandleEntry> = t.handles.iter_mut().filter_map(Option::take).collect();
        handles.into_iter().for_each(release);
        if let ExitStatus::Code(code) = status {
            kprintln!("ZOO_OS PROC exit pid={} name={} code={}", pid, name, code);
        }
        if self.display_owner == Some(pid) {
            self.display_owner = None;
            DISPLAY_OWNED.store(false, Ordering::Relaxed);
            console::resume();
            kprintln!("ZOO_OS DISPLAY released pid={}", pid);
        }
        self.exits.push((pid, status));
        self.zombies.push(i);
        self.wake_all();
    }

    /// Frees exited tasks that are not the current one.
    fn reap(&mut self, mem: &mut Memory) {
        let current = self.current;
        let mut keep = Vec::new();
        for i in core::mem::take(&mut self.zombies) {
            if i == current {
                keep.push(i);
                continue;
            }
            let t = self.tasks[i].take().expect("zombie task present");
            if let Some(p4) = t.p4 {
                if t.display_pages > 0 {
                    // Device memory: unmap, never hand to the frame allocator.
                    mem.unmap_user_keep(p4, DISPLAY_BASE, t.display_pages);
                }
                mem.free_address_space(p4);
            }
            if let Some((slot, _)) = t.kstack {
                mem.free_kernel_stack(slot);
                self.kstack_used[slot] = false;
            }
        }
        self.zombies = keep;
    }

    /// Makes task `i` current and returns the frame to resume. Re-runs a
    /// pending system call first; if it blocks or exits again, moves on.
    fn activate(&mut self, mut i: usize, mem: &mut Memory) -> *mut TrapFrame {
        loop {
            let (cr3, _) = Cr3::read();
            let flags = Cr3::read().1;
            let t = self.task(i);
            let want = t.p4.unwrap_or(mem.kernel_p4);
            if let Some((_, top)) = t.kstack {
                // SAFETY: interrupts are disabled (we are in the trap
                // dispatcher) and `top` is this task's mapped kernel stack.
                unsafe { gdt::set_kernel_stack(top) };
            }
            if cr3 != want {
                // SAFETY: `want` is either the kernel's table or a live
                // process table that maps the whole kernel half identically,
                // so the code and stack in use stay mapped across the switch.
                unsafe { Cr3::write(want, flags) };
            }
            self.current = i;
            let t = self.task(i);
            let frame = t.frame as *mut TrapFrame;
            if !t.restart {
                return frame;
            }
            t.restart = false;
            // SAFETY: a blocked task's saved frame stays valid on its own
            // kernel stack until it is resumed, which is now.
            let f = unsafe { &mut *frame };
            match self.syscall(i, f, mem) {
                Outcome::Ret(v) => {
                    f.rax = v as u64;
                    return frame;
                }
                Outcome::Yield => {
                    f.rax = 0;
                    return frame;
                }
                Outcome::Block => {
                    let t = self.task(i);
                    t.state = State::Blocked;
                    t.restart = true;
                }
                Outcome::Exit(code) => self.finish(i, ExitStatus::Code(code)),
            }
            i = self.pick_next();
        }
    }

    /// Runs system call `f.rax` for task `i`, whose address space is active.
    fn syscall(&mut self, i: usize, f: &mut TrapFrame, mem: &mut Memory) -> Outcome {
        let (number, a1, a2, a3) = (f.rax, f.rdi, f.rsi, f.rdx);
        let p4 = self.task(i).p4.expect("system call from a user task");
        // A user range the kernel may read (or write) right now.
        let ok = |addr: u64, len: u64, write: bool| mem.user_range_ok(p4, addr, len, write);
        match number {
            sys::ABI_VERSION => Outcome::Ret(ABI_VERSION as i64),
            sys::EXIT if a1 > 255 => err(Error::Inval),
            sys::EXIT => Outcome::Exit(a1 as u8),
            sys::YIELD => Outcome::Yield,
            sys::LOG => {
                if a2 > MAX_MESSAGE as u64 {
                    return err(Error::Inval);
                }
                if !ok(a1, a2, false) {
                    return err(Error::Fault);
                }
                // SAFETY: the range was just checked to be mapped, readable
                // user memory in the active address space; user code is not
                // running while we read it.
                let bytes = unsafe { core::slice::from_raw_parts(a1 as *const u8, a2 as usize) };
                let t = self.task(i);
                log_user(t.pid, &t.name, bytes);
                Outcome::Ret(a2 as i64)
            }
            sys::CHANNEL_CREATE => {
                if !ok(a1, 16, true) {
                    return err(Error::Fault);
                }
                let t = self.task(i);
                if t.free_slots() < 2 {
                    return err(Error::NoMem);
                }
                let chan = Arc::new(Mutex::new(Channel {
                    queues: [VecDeque::new(), VecDeque::new()],
                    open: [true, true],
                }));
                let rights = RIGHT_SEND | RIGHT_RECV;
                let end = |side| HandleEntry {
                    object: Object::Endpoint {
                        chan: chan.clone(),
                        side,
                    },
                    rights,
                };
                let h0 = t.insert(end(0)).expect("checked free slots");
                let h1 = t.insert(end(1)).expect("checked free slots");
                // SAFETY: 16 writable user bytes, checked above; unaligned
                // writes because user pointers need not be aligned.
                unsafe {
                    (a1 as *mut u64).write_unaligned(h0);
                    ((a1 + 8) as *mut u64).write_unaligned(h1);
                }
                Outcome::Ret(0)
            }
            sys::SPAWN => {
                if a2 == 0 || a2 > MAX_PATH as u64 {
                    return err(Error::Inval);
                }
                if !ok(a1, a2, false) {
                    return err(Error::Fault);
                }
                // SAFETY: checked readable user range in the active space.
                let raw = unsafe { core::slice::from_raw_parts(a1 as *const u8, a2 as usize) };
                let Ok(name) = core::str::from_utf8(raw) else {
                    return err(Error::Inval);
                };
                let name = String::from(name);
                let t = self.task(i);
                if t.free_slots() < 1 {
                    return err(Error::NoMem);
                }
                let give = if a3 == NO_HANDLE {
                    None
                } else {
                    match t.get(a3) {
                        Some(h) => Some(h.clone()),
                        None => return err(Error::BadHandle),
                    }
                };
                match self.spawn(mem, &name, give) {
                    Ok((_, pid, _)) => {
                        let t = self.task(i);
                        if a3 != NO_HANDLE {
                            // Moved into the child: removed here, not released.
                            let _ = t.take(a3);
                        }
                        let h = t
                            .insert(HandleEntry {
                                object: Object::Process { pid },
                                rights: RIGHT_WAIT | RIGHT_KILL,
                            })
                            .expect("checked a free slot");
                        Outcome::Ret(h as i64)
                    }
                    Err(e) => err(e),
                }
            }
            sys::SEND => {
                if a3 > MAX_MESSAGE as u64 {
                    return err(Error::Inval);
                }
                let (chan, side) = match self.task(i).get(a1) {
                    None => return err(Error::BadHandle),
                    Some(HandleEntry {
                        object: Object::Endpoint { chan, side },
                        rights,
                    }) if rights & RIGHT_SEND != 0 => (chan.clone(), *side),
                    Some(_) => return err(Error::Rights),
                };
                if !ok(a2, a3, false) {
                    return err(Error::Fault);
                }
                {
                    let mut c = chan.lock();
                    let peer = 1 - side;
                    if !c.open[peer] {
                        return err(Error::PeerClosed);
                    }
                    if c.queues[peer].len() >= CHANNEL_DEPTH {
                        return err(Error::Full);
                    }
                    // SAFETY: checked readable user range in the active space.
                    let msg = unsafe { core::slice::from_raw_parts(a2 as *const u8, a3 as usize) };
                    c.queues[peer].push_back(msg.to_vec());
                }
                self.wake_all();
                Outcome::Ret(0)
            }
            sys::RECV | sys::TRY_RECV => {
                let (chan, side) = match self.task(i).get(a1) {
                    None => return err(Error::BadHandle),
                    Some(HandleEntry {
                        object: Object::Endpoint { chan, side },
                        rights,
                    }) if rights & RIGHT_RECV != 0 => (chan.clone(), *side),
                    Some(_) => return err(Error::Rights),
                };
                if !ok(a2, a3, true) {
                    return err(Error::Fault);
                }
                let mut c = chan.lock();
                let Some(len) = c.queues[side].front().map(Vec::len) else {
                    return if !c.open[1 - side] {
                        err(Error::PeerClosed)
                    } else if number == sys::TRY_RECV {
                        err(Error::Again)
                    } else {
                        Outcome::Block
                    };
                };
                if len as u64 > a3 {
                    return err(Error::TooSmall);
                }
                let msg = c.queues[side].pop_front().expect("front exists");
                // SAFETY: checked writable user range of `a3 >= len` bytes in
                // the active space.
                unsafe { core::ptr::copy_nonoverlapping(msg.as_ptr(), a2 as *mut u8, len) };
                Outcome::Ret(len as i64)
            }
            sys::WAIT => {
                let pid = match self.task(i).get(a1) {
                    None => return err(Error::BadHandle),
                    Some(HandleEntry {
                        object: Object::Process { pid },
                        rights,
                    }) if rights & RIGHT_WAIT != 0 => *pid,
                    Some(_) => return err(Error::Rights),
                };
                match self.exits.iter().find(|(p, _)| *p == pid) {
                    Some((_, ExitStatus::Code(c))) => Outcome::Ret(*c as i64),
                    Some((_, ExitStatus::Crashed)) => err(Error::Crashed),
                    Some((_, ExitStatus::Killed)) => err(Error::Killed),
                    None => Outcome::Block,
                }
            }
            sys::KILL => {
                let pid = match self.task(i).get(a1) {
                    None => return err(Error::BadHandle),
                    Some(HandleEntry {
                        object: Object::Process { pid },
                        rights,
                    }) if rights & RIGHT_KILL != 0 => *pid,
                    Some(_) => return err(Error::Rights),
                };
                let by = self.task(i).pid;
                let target = self
                    .tasks
                    .iter()
                    .position(|t| matches!(t, Some(t) if t.pid == pid && t.state != State::Exited));
                match target {
                    // Killing yourself is an exit.
                    Some(j) if j == i => Outcome::Exit(0),
                    Some(j) => {
                        let name = self.task(j).name.clone();
                        kprintln!("ZOO_OS PROC killed pid={} name={} by={}", pid, name, by);
                        self.finish(j, ExitStatus::Killed);
                        Outcome::Ret(0)
                    }
                    // Already ended: nothing to do.
                    None => Outcome::Ret(0),
                }
            }
            sys::TICKS => Outcome::Ret(interrupts::ticks() as i64),
            sys::SLEEP => {
                let now = interrupts::ticks();
                let t = self.task(i);
                let deadline = *t.sleep_until.get_or_insert(now.saturating_add(a1));
                if now >= deadline {
                    t.sleep_until = None;
                    Outcome::Ret(0)
                } else {
                    Outcome::Block
                }
            }
            sys::DISPLAY_ACQUIRE => {
                let size = core::mem::size_of::<DisplayInfo>() as u64;
                if !ok(a1, size, true) {
                    return err(Error::Fault);
                }
                let Some((virt, len, fb)) = console::framebuffer_region() else {
                    return err(Error::NotFound);
                };
                let format = match fb.pixel_format {
                    PixelFormat::Rgb => FORMAT_RGB,
                    PixelFormat::Bgr => FORMAT_BGR,
                    _ => return err(Error::Inval),
                };
                let me = self.task(i).pid;
                if let Some(owner) = self.display_owner
                    && owner != me
                    && self
                        .tasks
                        .iter()
                        .flatten()
                        .any(|t| t.pid == owner && t.state != State::Exited)
                {
                    return err(Error::Busy);
                }
                let pages = (len as u64).div_ceil(4096);
                if self.task(i).display_pages == 0 {
                    for k in 0..pages {
                        let Some(phys) = mem.mapper.translate_addr(VirtAddr::new(virt + k * 4096))
                        else {
                            mem.unmap_user_keep(p4, DISPLAY_BASE, k);
                            return err(Error::Inval);
                        };
                        let frame = PhysFrame::containing_address(phys);
                        if mem
                            .map_user_frame(p4, DISPLAY_BASE + k * 4096, frame, true)
                            .is_err()
                        {
                            mem.unmap_user_keep(p4, DISPLAY_BASE, k);
                            return err(Error::NoMem);
                        }
                    }
                    self.task(i).display_pages = pages;
                }
                self.display_owner = Some(me);
                DISPLAY_OWNED.store(true, Ordering::Relaxed);
                console::suspend();
                let name = self.task(i).name.clone();
                kprintln!("ZOO_OS DISPLAY acquired pid={} name={}", me, name);
                let info = DisplayInfo {
                    base: DISPLAY_BASE,
                    width: fb.width as u64,
                    height: fb.height as u64,
                    stride: fb.stride as u64,
                    bytes_per_pixel: fb.bytes_per_pixel as u64,
                    format,
                };
                // SAFETY: checked writable user range of `size` bytes in the
                // active space; unaligned because user pointers may be.
                unsafe { (a1 as *mut DisplayInfo).write_unaligned(info) };
                Outcome::Ret(0)
            }
            sys::INPUT_READ => {
                let me = self.task(i).pid;
                if self.display_owner != Some(me) {
                    return err(Error::Rights);
                }
                if !ok(a1, a2, true) {
                    return err(Error::Fault);
                }
                let mut n = 0u64;
                while n < a2 {
                    let Some(b) = keyboard::pop() else { break };
                    // SAFETY: inside the writable user range checked above.
                    unsafe { ((a1 + n) as *mut u8).write(b) };
                    n += 1;
                }
                Outcome::Ret(n as i64)
            }
            sys::FS_STAT => {
                let size = core::mem::size_of::<FsEntry>() as u64;
                if !ok(a2, size, true) {
                    return err(Error::Fault);
                }
                let Some((name, len, _)) = initrd::entry(a1 as usize) else {
                    return err(Error::NotFound);
                };
                let mut e = FsEntry {
                    size: len as u64,
                    ..FsEntry::default()
                };
                let n = name.len().min(MAX_FS_NAME);
                e.name[..n].copy_from_slice(&name.as_bytes()[..n]);
                e.name_len = n as u64;
                // SAFETY: checked writable user range of `size` bytes.
                unsafe { (a2 as *mut FsEntry).write_unaligned(e) };
                Outcome::Ret(0)
            }
            sys::CLOSE => match self.task(i).take(a1) {
                Some(entry) => {
                    release(entry);
                    self.wake_all();
                    Outcome::Ret(0)
                }
                None => err(Error::BadHandle),
            },
            _ => err(Error::NoSys),
        }
    }

    /// Saves the current frame and switches to the next ready task.
    fn switch_away(&mut self, frame: *mut TrapFrame, mem: &mut Memory) -> *mut TrapFrame {
        let cur = self.current;
        self.task(cur).frame = frame as u64;
        let next = self.pick_next();
        if next == cur && self.task(cur).state == State::Ready {
            return frame;
        }
        self.activate(next, mem)
    }
}

/// Copies each segment's file bytes into freshly mapped user pages.
fn load_image(mem: &mut Memory, p4: PhysFrame, file: &[u8], image: &elf::Image) -> Result<(), ()> {
    for seg in image.loadable() {
        let (start, end) = seg.page_range();
        let mut page = start;
        while page < end {
            let frame = mem
                .map_user_page(p4, page, seg.writable(), seg.executable())
                .map_err(|_| ())?;
            // Part of the segment's file data that falls in this page.
            let data_start = seg.vaddr.max(page);
            let data_end = (seg.vaddr + seg.file_size).min(page + 4096);
            if data_start < data_end {
                let src = (seg.file_offset + (data_start - seg.vaddr)) as usize;
                let n = (data_end - data_start) as usize;
                let dst = mem.phys_to_virt(frame.start_address());
                // SAFETY: `frame` is a fresh page owned by this space, reached
                // through the window; `src..src+n` lies in `file` (the parser
                // checked offset + file size) and `n` fits in the page.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        file[src..src + n].as_ptr(),
                        dst.add((data_start - page) as usize),
                        n,
                    )
                };
            }
            page += 4096;
        }
    }
    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    let mut page = stack_bottom;
    while page < USER_STACK_TOP {
        mem.map_user_page(p4, page, true, false).map_err(|_| ())?;
        page += 4096;
    }
    Ok(())
}

/// Prints user log text as `[pid:name] text`, so it can never be mistaken
/// for a kernel marker: `ZOO_OS` becomes `ZOO-OS`, and control or
/// non-ASCII bytes other than newline become `?`.
fn log_user(pid: u64, name: &str, bytes: &[u8]) {
    for line in bytes.split(|&b| b == b'\n') {
        let mut text = String::with_capacity(line.len());
        for &b in line {
            text.push(if (0x20..0x7F).contains(&b) {
                b as char
            } else {
                '?'
            });
        }
        let text = text.replace("ZOO_OS", "ZOO-OS");
        kprintln!("[{}:{}] {}", pid, name, text);
    }
}

// ---- trap entry points (called from trap::trap_dispatch) --------------

/// Locks for the dispatcher. Interrupts are already off; a held lock here
/// means a kernel bug, so say so rather than deadlock.
fn dispatch_locked<R>(f: impl FnOnce(&mut Scheduler, &mut Memory) -> R) -> Option<R> {
    let mut g = SCHED
        .try_lock()
        .expect("scheduler lock held when a trap arrived");
    let s = g.as_mut()?;
    Some(memory::with(|m| f(s, m)))
}

pub fn on_timer(frame: *mut TrapFrame) -> *mut TrapFrame {
    dispatch_locked(|s, m| {
        s.reap(m);
        let now = interrupts::ticks();
        for t in s.tasks.iter_mut().flatten() {
            if t.state == State::Blocked && t.sleep_until.is_some_and(|d| now >= d) {
                t.state = State::Ready;
            }
        }
        if s.current == 0 && !KERNEL_IDLE.load(Ordering::Relaxed) {
            return frame;
        }
        s.switch_away(frame, m)
    })
    .unwrap_or(frame)
}

pub fn on_syscall(frame: *mut TrapFrame) -> *mut TrapFrame {
    dispatch_locked(|s, m| {
        let cur = s.current;
        s.task(cur).frame = frame as u64;
        // SAFETY: the frame was just pushed for this trap on this task's
        // kernel stack.
        let f = unsafe { &mut *frame };
        match s.syscall(cur, f, m) {
            Outcome::Ret(v) => {
                f.rax = v as u64;
                frame
            }
            Outcome::Yield => {
                f.rax = 0;
                s.switch_away(frame, m)
            }
            Outcome::Block => {
                let t = s.task(cur);
                t.state = State::Blocked;
                t.restart = true;
                s.switch_away(frame, m)
            }
            Outcome::Exit(code) => {
                s.finish(cur, ExitStatus::Code(code));
                s.switch_away(frame, m)
            }
        }
    })
    .expect("system call before the scheduler started")
}

pub fn on_user_fault(frame: *mut TrapFrame) -> *mut TrapFrame {
    dispatch_locked(|s, m| {
        let cur = s.current;
        // SAFETY: the frame was just pushed for this trap.
        let f = unsafe { &*frame };
        let t = s.task(cur);
        let mut line = String::new();
        let _ = write!(line, "ZOO_OS PROC crash pid={} name={} ", t.pid, t.name);
        if f.vector == trap::VEC_PAGE_FAULT {
            let (access, cause) = interrupts::page_fault_kind(f.error);
            let _ = write!(
                line,
                "fault=page-fault access={} cause={} addr={:#018x}",
                access,
                cause,
                Cr2::read_raw()
            );
        } else {
            let _ = write!(
                line,
                "fault={} rip={:#018x}",
                interrupts::fault_name(f.vector),
                f.rip
            );
        }
        kprintln!("{}", line);
        kprintln!(
            "ZOO_OS INFO crash rip={:#018x} rsp={:#018x} error={:#x}",
            f.rip,
            f.rsp,
            f.error
        );
        s.finish(cur, ExitStatus::Crashed);
        s.switch_away(frame, m)
    })
    .expect("user fault before the scheduler started")
}
