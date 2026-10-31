//! Processes, scheduling, handles, channels and system calls
//! (docs/m3-design.md §3–§6; display, input, sleep and files added by
//! docs/m4-design.md §2).
//!
//! All state lives in `SCHED`, locked only with interrupts disabled: from the
//! trap dispatcher (interrupts are off there) or from kernel-task code inside
//! `without_interrupts`. Lock order: `SCHED`, then `memory::with`.

use crate::console;
use crate::fpu::{self, FxArea};
use crate::gdt;
use crate::initrd;
use crate::interrupts;
use crate::keyboard;
use crate::klog;
use crate::kprintln;
use crate::memory::{
    self, KSTACK_SLOTS, Memory, USER_IMAGE_END, USER_IMAGE_START, USER_STACK_SIZE, USER_STACK_TOP,
};
use crate::mouse;
use crate::rtc;
use crate::trap::{self, TrapFrame};
use crate::virtio_blk;
use alloc::boxed::Box;
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
use x86_64::registers::model_specific::FsBase;
use x86_64::structures::paging::{PhysFrame, Translate};
use zoo_abi::BLOCK_BYTES;
use zoo_abi::{
    ABI_VERSION, CHANNEL_DEPTH, DISPLAY_BASE, DisplayInfo, EXC_RESUME, EXC_UNHANDLED, Error,
    ExceptionContext, FORMAT_BGR, FORMAT_RGB, FsEntry, HEAP_BASE, HEAP_LIMIT, HandleMsg, MAP_BASE,
    MAX_FS_NAME, MAX_HANDLES, MAX_MESSAGE, MAX_PATH, MapInfo, NO_HANDLE, RECV_NONBLOCK,
    VM_COMMIT_LIMIT, VM_COMMITTED, VM_END, VM_FREE, VM_GRANULE, VM_MAX_RESERVATIONS, VM_RESERVED,
    VM_START, VmInfo, elf, prot, sys,
};
use zoo_abi::{CLOCK_MONOTONIC, CLOCK_REALTIME, MAX_CMDLINE, MouseEvent};
use zoo_abi::{WAIT_ANY_MAX, WAIT_FOREVER};

/// Kernel task plus one task per kernel-stack slot.
const MAX_TASKS: usize = 1 + KSTACK_SLOTS;
const RIGHT_SEND: u8 = 1;
const RIGHT_RECV: u8 = 2;
const RIGHT_WAIT: u8 = 4;
const RIGHT_KILL: u8 = 8;
const RFLAGS_IF: u64 = 1 << 9;
/// First id given to an additional thread.
const FIRST_EXTRA_TID: u64 = 0x1_0000;
/// User addresses are below this (the canonical lower half).
const USER_HALF_END: u64 = memory::USER_END;
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
    queues: [VecDeque<Msg>; 2],
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
    /// A thread of the same process (docs/posix1-design.md).
    Thread {
        tid: u64,
    },
    /// Reads the kernel log buffer (docs/s2-design.md).
    LogReader,
    /// The data disk (docs/s5-design.md).
    Block,
}

#[derive(Clone)]
struct HandleEntry {
    object: Object,
    rights: u8,
}

/// A channel message: bytes, or one handle in transit with a tag
/// (docs/s3-design.md).
enum Msg {
    Bytes(Vec<u8>),
    Handle(HandleEntry, u64),
}

/// Releases what a handle held. Channel ends close (the peer sees
/// `PEERCLOSED`), and handles still queued towards the closed end are
/// released in turn; process and directory handles hold nothing.
fn release(entry: HandleEntry) {
    if let Object::Endpoint { chan, side } = entry.object {
        // Take the queue out under the lock, release its handles after, so
        // a queued end of this same channel cannot deadlock.
        let queued: VecDeque<Msg> = {
            let mut c = chan.lock();
            c.open[side] = false;
            core::mem::take(&mut c.queues[side])
        };
        for m in queued {
            if let Msg::Handle(e, _) = m {
                release(e);
            }
        }
    }
}

/// What the threads of one process share: address space, handles and
/// memory regions (docs/posix1-design.md §2).
struct Proc {
    pid: u64,
    name: String,
    p4: PhysFrame,
    handles: [Option<HandleEntry>; MAX_HANDLES],
    generation: [u16; MAX_HANDLES],
    /// Framebuffer pages mapped at `DISPLAY_BASE` (0 if none).
    display_pages: u64,
    /// Next free heap address (`mem_alloc`).
    heap_next: u64,
    /// Next free address in the file-mapping region (`fs_map`).
    map_next: u64,
    /// Read-only file mappings of ramdisk pages: (address, pages). Never
    /// freed through the frame allocator; unmapped before the space is freed.
    file_maps: Vec<(u64, u64)>,
    /// May call `log_attach` and `block_attach` (the resident supervisor).
    log_ok: bool,
    /// Program name and arguments, NUL-separated (`args_get`).
    cmdline: String,
    /// Threads that have not exited.
    live: usize,
    /// Threads not yet reaped; the address space is freed when this is 0.
    members: usize,
    /// Set once the process has ended (exit, crash or kill).
    ended: bool,
    /// Address-space reservations (start, len), sorted by start
    /// (docs/posix2-design.md).
    vmas: Vec<(u64, u64)>,
    /// Linux programs: the `brk` area (base, current break), and open
    /// files (descriptor 3 is index 0): initrd data and read position
    /// (docs/linux2-design.md).
    linux_brk: Option<(u64, u64)>,
    linux_files: Vec<Option<(&'static [u8], u64)>>,
    /// Pages committed through `vm_commit`.
    committed: u64,
    /// User exception handler entry (docs/posix3-design.md); 0 if none.
    exc_handler: u64,
}

/// One thread. Each user thread belongs to a `Proc`; the kernel task does not.
struct Task {
    /// The process id (copied from the `Proc`, for logs and lookups).
    pid: u64,
    /// Thread id, unique across the system; the first thread's equals `pid`.
    tid: u64,
    name: String,
    /// Index into `Scheduler::procs`; `None` for the kernel task.
    proc: Option<usize>,
    state: State,
    /// Saved `*mut TrapFrame` (as an address), valid while not running.
    frame: u64,
    /// Re-run the system call in `frame` when next scheduled.
    restart: bool,
    /// The process's page table (copied); `None` for the kernel task.
    p4: Option<PhysFrame>,
    /// (slot, stack top); `None` for the kernel task.
    kstack: Option<(usize, u64)>,
    /// Deadline (in ticks) of an unfinished `sleep` or timed `futex_wait`.
    sleep_until: Option<u64>,
    /// x87/SSE register state while switched out; `None` for the kernel.
    fpu: Option<Box<FxArea>>,
    /// User FS base (thread-local storage), loaded when the thread runs.
    fs_base: u64,
    /// Address this thread waits on in `futex_wait`.
    futex: Option<u64>,
    /// Set by `futex_wake`; the restarted `futex_wait` then returns 0.
    futex_woken: bool,
    /// Top of this thread's exception stack (0: below the faulting `rsp`).
    exc_stack: u64,
    /// Running its exception handler; a fault now is fatal.
    in_exception: bool,
}

impl Task {
    fn new(pid: u64, tid: u64, name: &str) -> Task {
        Task {
            pid,
            tid,
            name: String::from(name),
            proc: None,
            state: State::Ready,
            frame: 0,
            restart: false,
            p4: None,
            kstack: None,
            sleep_until: None,
            fpu: None,
            fs_base: 0,
            futex: None,
            futex_woken: false,
            exc_stack: 0,
            in_exception: false,
        }
    }
}

impl Proc {
    fn new(pid: u64, name: &str, p4: PhysFrame) -> Proc {
        Proc {
            pid,
            name: String::from(name),
            cmdline: String::from(name),
            p4,
            handles: [const { None }; MAX_HANDLES],
            generation: [0; MAX_HANDLES],
            display_pages: 0,
            heap_next: HEAP_BASE,
            map_next: MAP_BASE,
            file_maps: Vec::new(),
            log_ok: false,
            live: 1,
            members: 1,
            ended: false,
            vmas: Vec::new(),
            linux_brk: None,
            linux_files: Vec::new(),
            committed: 0,
            exc_handler: 0,
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
    /// This thread ends; the process ends with it if it was the last.
    ThreadExit(u8),
    /// Return to user mode with the frame as the call left it (`rax`
    /// included): `exception_return`.
    Resume,
    /// The process crashes (a handler declared its exception unhandled).
    Crash,
}

fn err(e: Error) -> Outcome {
    Outcome::Ret(e.as_return())
}

struct Scheduler {
    tasks: Vec<Option<Task>>,
    procs: Vec<Option<Proc>>,
    /// (pid, tid, code) of exited threads, for `wait` on a thread handle;
    /// dropped when the process is freed.
    thread_exits: Vec<(u64, u64, u8)>,
    current: usize,
    next_pid: u64,
    /// Ids for threads after a process's first (whose id is the pid), from
    /// their own range so process ids do not depend on thread counts.
    next_tid: u64,
    exits: Vec<(u64, ExitStatus)>,
    zombies: Vec<usize>,
    kstack_used: [bool; KSTACK_SLOTS],
    /// Pid of the process that owns the display and keyboard.
    display_owner: Option<u64>,
    /// The kernel's end of the current desktop session's control channel,
    /// with that session's Zookeeper pid.
    control: Option<(u64, HandleEntry)>,
    /// The kernel's end of the resident supervisor's control channel, with
    /// its pid (docs/s1-design.md).
    supervisor: Option<(u64, HandleEntry)>,
    /// Prefer a ready user task over the idling kernel task (true except
    /// while handling a timer tick).
    prefer_user: bool,
    /// A home-folder handle the supervisor sent for the next desktop
    /// session (docs/s4-design.md §3).
    pending_home: Option<HandleEntry>,
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
        tasks.push(Some(Task::new(0, 0, "kernel")));
        *SCHED.lock() = Some(Scheduler {
            tasks,
            procs: Vec::new(),
            thread_exits: Vec::new(),
            current: 0,
            next_pid: 1,
            next_tid: FIRST_EXTRA_TID,
            exits: Vec::new(),
            zombies: Vec::new(),
            kstack_used: [false; KSTACK_SLOTS],
            display_owner: None,
            control: None,
            supervisor: None,
            prefer_user: true,
            pending_home: None,
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
    with_sched(|s, m| {
        let (_, pid) = s.spawn(m, name, [None, None])?;
        Ok(pid)
    })
}

/// Starts a Linux program from `linux/` in the initrd (shell `linux`).
pub fn spawn_linux_from_kernel(cmdline: &str) -> Result<u64, Error> {
    with_sched(|s, m| {
        let (_, pid) = s.spawn_as(m, cmdline, [None, None], true)?;
        Ok(pid)
    })
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

/// A channel between the kernel (side 0) and a new Zookeeper (side 1) whose
/// first message is `mode`.
fn control_channel(mode: &[u8]) -> (HandleEntry, HandleEntry) {
    let chan = Arc::new(Mutex::new(Channel {
        queues: [VecDeque::new(), VecDeque::new()],
        open: [true, true],
    }));
    // Side 1 is Zookeeper's: its receive queue holds the mode.
    chan.lock().queues[1].push_back(Msg::Bytes(mode.to_vec()));
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
    (kernel_end, zookeeper_end)
}

/// Starts the resident supervisor: Zookeeper in supervise mode
/// (docs/s1-design.md). Returns its pid.
pub fn start_supervisor() -> Result<u64, Error> {
    with_sched(|s, m| {
        if s.supervisor.is_some() {
            return Err(Error::Busy);
        }
        let (kernel_end, zookeeper_end) = control_channel(b"supervise");
        let (i, pid) = s.spawn(m, "zookeeper", [Some(zookeeper_end), None])?;
        s.proc(i).log_ok = true;
        s.supervisor = Some((pid, kernel_end));
        Ok(pid)
    })
}

/// Sends `msg` to the supervisor. `PEERCLOSED` once it has ended, `FULL`
/// when its queue is full.
pub fn supervisor_send(msg: &[u8]) -> Result<(), Error> {
    with_sched(|s, _| {
        let Some((
            _,
            HandleEntry {
                object: Object::Endpoint { chan, .. },
                ..
            },
        )) = &s.supervisor
        else {
            return Err(Error::NotFound);
        };
        let r = {
            let mut c = chan.lock();
            if !c.open[1] {
                Err(Error::PeerClosed)
            } else if c.queues[1].len() >= CHANNEL_DEPTH {
                Err(Error::Full)
            } else {
                c.queues[1].push_back(Msg::Bytes(msg.to_vec()));
                Ok(())
            }
        };
        s.wake_all();
        r
    })
}

/// The next message from the supervisor, if one is waiting.
pub fn supervisor_try_recv() -> Option<Vec<u8>> {
    with_sched(|s, _| {
        let Some((
            _,
            HandleEntry {
                object: Object::Endpoint { chan, .. },
                ..
            },
        )) = &s.supervisor
        else {
            return None;
        };
        let m = chan.lock().queues[0].pop_front();
        if m.is_some() {
            s.wake_all();
        }
        match m {
            Some(Msg::Bytes(b)) => Some(b),
            // A handle is kept for the next desktop session (the only
            // handle the supervisor sends the kernel); reported as `<handle>`.
            Some(Msg::Handle(e, _)) => {
                if let Some(old) = s.pending_home.replace(e) {
                    release(old);
                }
                Some(b"<handle>".to_vec())
            }
            None => None,
        }
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
        let (kernel_end, zookeeper_end) = control_channel(b"session");
        let home = s.pending_home.take();
        let (_, pid) = s.spawn(m, "zookeeper", [Some(zookeeper_end), home])?;
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

    /// The process task `i` belongs to (a user task).
    fn proc(&mut self, i: usize) -> &mut Proc {
        let p = self.task(i).proc.expect("user task");
        self.procs[p].as_mut().expect("process slot in use")
    }

    /// A free task slot and kernel stack slot, or `NOMEM`.
    fn free_task_slots(&mut self) -> Result<(usize, usize), Error> {
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
        Ok((index, slot))
    }

    /// Writes a first user frame at the top of kernel stack `top`.
    fn first_frame(top: u64, rip: u64, rsp: u64, rdi: u64, rsi: u64) -> u64 {
        let sel = gdt::selectors();
        let frame = TrapFrame {
            rip,
            cs: sel.user_code.0 as u64,
            rflags: RFLAGS_IF | RFLAGS_RESERVED,
            rsp,
            ss: sel.user_data.0 as u64,
            rdi,
            rsi,
            ..TrapFrame::default()
        };
        let frame_addr = top - core::mem::size_of::<TrapFrame>() as u64;
        // SAFETY: the kernel stack `[top - KSTACK_SIZE, top)` was just mapped
        // writable and belongs to this new task alone; the frame fits at its
        // top.
        unsafe { (frame_addr as *mut TrapFrame).write(frame) };
        frame_addr
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
    ///
    /// On a voluntary switch (a task blocked, yielded or ended) a ready user
    /// task is preferred over the kernel task: the kernel task only idles
    /// (`hlt` until the next interrupt), and picking it would delay the
    /// woken task by up to a whole tick (docs/s4a-design.md). On timer
    /// ticks the plain round-robin applies, so the kernel shell still gets
    /// its turn while user tasks are busy.
    fn pick_next(&self) -> usize {
        let n = self.tasks.len();
        let ready = |i: usize| matches!(&self.tasks[i], Some(t) if t.state == State::Ready);
        let mut order = (1..=n).map(|k| (self.current + k) % n);
        if self.prefer_user
            && let Some(i) = order.clone().find(|&i| i != 0 && ready(i))
        {
            return i;
        }
        order.find(|&i| ready(i)).unwrap_or(0)
    }

    /// Loads, maps and readies the program a command line names (the part
    /// before the first NUL; the rest are its arguments); `give` become its
    /// start-up handles (in `rdi` and `rsi`, `NO_HANDLE` where absent).
    /// Returns (task index, pid).
    fn spawn(
        &mut self,
        mem: &mut Memory,
        cmdline: &str,
        give: [Option<HandleEntry>; 2],
    ) -> Result<(usize, u64), Error> {
        self.spawn_as(mem, cmdline, give, false)
    }

    /// `spawn`, or (`linux`) a Linux program from `linux/` in the initrd,
    /// started with the System V process stack (docs/linux1-design.md).
    fn spawn_as(
        &mut self,
        mem: &mut Memory,
        cmdline: &str,
        give: [Option<HandleEntry>; 2],
        linux: bool,
    ) -> Result<(usize, u64), Error> {
        let name = cmdline.split('\0').next().unwrap_or("");
        if cmdline.len() > MAX_CMDLINE || !valid_name(name) || name.len() > MAX_PATH {
            return Err(Error::Inval);
        }
        let mut path = String::from(if linux { "linux/" } else { "bin/" });
        path.push_str(name);
        let file = initrd::find(&path).ok_or(Error::NotFound)?;
        let image = elf::parse(file, USER_IMAGE_START, USER_IMAGE_END).map_err(|e| {
            kprintln!("ZOO_OS INFO spawn {} rejected: {:?}", name, e);
            Error::Inval
        })?;
        let (index, slot) = self.free_task_slots()?;
        let p4 = mem.new_address_space().map_err(|_| Error::NoMem)?;
        let Ok(stack_page) = load_image(mem, p4, file, &image) else {
            mem.free_address_space(p4);
            return Err(Error::NoMem);
        };
        let rsp = if linux {
            linux_stack(mem, stack_page, cmdline, file, &image)
        } else {
            USER_STACK_TOP - 8
        };
        let Ok(top) = mem.map_kernel_stack(slot) else {
            mem.free_address_space(p4);
            return Err(Error::NoMem);
        };
        self.kstack_used[slot] = true;

        let pid = self.next_pid;
        self.next_pid += 1;
        let mut proc = Proc::new(pid, name, p4);
        proc.cmdline = String::from(cmdline);
        let [arg, arg2] = give.map(|g| match g {
            Some(entry) => proc
                .insert(entry)
                .expect("new process has free handle slots"),
            None => NO_HANDLE,
        });
        let pidx = match self.procs.iter().position(|p| p.is_none()) {
            Some(k) => k,
            None => {
                self.procs.push(None);
                self.procs.len() - 1
            }
        };
        self.procs[pidx] = Some(proc);
        let mut task = Task::new(pid, pid, name);
        // SysV: at function entry, rsp + 8 is 16-byte aligned.
        task.frame = Self::first_frame(top, image.entry, rsp, arg, arg2);
        task.fpu = Some(Box::new(fpu::new_state()));
        task.p4 = Some(p4);
        task.proc = Some(pidx);
        task.kstack = Some((slot, top));
        self.tasks[index] = Some(task);
        kprintln!("ZOO_OS PROC start pid={} name={}", pid, name);
        Ok((index, pid))
    }

    /// Ends the whole process task `i` belongs to: every thread stops, its
    /// handles close, the status is recorded and waiters wake. Memory is
    /// reclaimed later by `reap`, off the threads' own stacks.
    fn finish(&mut self, i: usize, status: ExitStatus) {
        let Some(pidx) = self.task(i).proc else {
            return;
        };
        let (pid, name, handles) = {
            let p = self.procs[pidx].as_mut().expect("process slot in use");
            if p.ended {
                return;
            }
            p.ended = true;
            p.live = 0;
            let handles: Vec<HandleEntry> = p.handles.iter_mut().filter_map(Option::take).collect();
            (p.pid, p.name.clone(), handles)
        };
        for k in 0..self.tasks.len() {
            if let Some(t) = &mut self.tasks[k]
                && t.proc == Some(pidx)
                && t.state != State::Exited
            {
                t.state = State::Exited;
                t.restart = false;
                self.zombies.push(k);
            }
        }
        handles.into_iter().for_each(release);
        if let ExitStatus::Code(code) = status {
            kprintln!("ZOO_OS PROC exit pid={} name={} code={}", pid, name, code);
        }
        if self.display_owner == Some(pid) {
            self.display_owner = None;
            DISPLAY_OWNED.store(false, Ordering::Relaxed);
            mouse::clear();
            console::resume();
            kprintln!("ZOO_OS DISPLAY released pid={}", pid);
        }
        self.exits.push((pid, status));
        self.wake_all();
    }

    /// Ends thread `i` only; the last thread's exit ends the process with
    /// the same code.
    fn finish_thread(&mut self, i: usize, code: u8) {
        let last = {
            let p = self.proc(i);
            p.live -= 1;
            p.live == 0
        };
        if last {
            self.finish(i, ExitStatus::Code(code));
            return;
        }
        let t = self.task(i);
        t.state = State::Exited;
        t.restart = false;
        let (pid, tid) = (t.pid, t.tid);
        kprintln!(
            "ZOO_OS INFO thread exit pid={} tid={} code={}",
            pid,
            tid,
            code
        );
        self.thread_exits.push((pid, tid, code));
        self.zombies.push(i);
        self.wake_all();
    }

    /// Frees exited tasks that are not the current one, and a process's
    /// memory once its last thread is freed.
    fn reap(&mut self, mem: &mut Memory) {
        let current = self.current;
        let mut keep = Vec::new();
        for i in core::mem::take(&mut self.zombies) {
            if i == current {
                keep.push(i);
                continue;
            }
            let t = self.tasks[i].take().expect("zombie task present");
            if let Some((slot, _)) = t.kstack {
                mem.free_kernel_stack(slot);
                self.kstack_used[slot] = false;
            }
            let Some(pidx) = t.proc else { continue };
            let p = self.procs[pidx].as_mut().expect("process slot in use");
            p.members -= 1;
            if p.members > 0 {
                continue;
            }
            let p = self.procs[pidx].take().expect("process slot in use");
            if p.display_pages > 0 {
                // Device memory: unmap, never hand to the frame allocator.
                mem.unmap_user_keep(p.p4, DISPLAY_BASE, p.display_pages);
            }
            for &(va, pages) in &p.file_maps {
                // Ramdisk pages: likewise never freed.
                mem.unmap_user_keep(p.p4, va, pages);
            }
            mem.free_address_space(p.p4);
            self.thread_exits.retain(|&(pid, _, _)| pid != p.pid);
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
            if t.p4.is_some() {
                // User FS base (thread-local storage). Kernel code does not
                // use FS, so it can stay loaded while in the kernel.
                FsBase::write(VirtAddr::new_truncate(t.fs_base));
            }
            let frame = t.frame as *mut TrapFrame;
            if !t.restart {
                if let Some(area) = &t.fpu {
                    fpu::restore(area);
                }
                return frame;
            }
            t.restart = false;
            // SAFETY: a blocked task's saved frame stays valid on its own
            // kernel stack until it is resumed, which is now.
            let f = unsafe { &mut *frame };
            let outcome = if f.vector == crate::trap::VEC_LINUX {
                self.linux_syscall(i, f, mem)
            } else {
                self.syscall(i, f, mem)
            };
            match outcome {
                Outcome::Ret(v) => {
                    f.rax = v as u64;
                    if let Some(area) = &self.task(i).fpu {
                        fpu::restore(area);
                    }
                    return frame;
                }
                Outcome::Yield => {
                    f.rax = 0;
                    if let Some(area) = &self.task(i).fpu {
                        fpu::restore(area);
                    }
                    return frame;
                }
                Outcome::Block => {
                    let t = self.task(i);
                    t.state = State::Blocked;
                    t.restart = true;
                }
                Outcome::Exit(code) => self.finish(i, ExitStatus::Code(code)),
                Outcome::ThreadExit(code) => self.finish_thread(i, code),
                // Neither blocks, so a restarted call never returns these.
                Outcome::Resume => return frame,
                Outcome::Crash => self.finish(i, ExitStatus::Crashed),
            }
            i = self.pick_next();
        }
    }

    /// A Linux x86-64 system call (docs/linux1-design.md): number in RAX,
    /// arguments in RDI, RSI, RDX, R10, R8, R9; errors as negative errno.
    fn linux_syscall(&mut self, i: usize, f: &mut TrapFrame, mem: &mut Memory) -> Outcome {
        let (number, a1, a2, a3) = (f.rax, f.rdi, f.rsi, f.rdx);
        let p4 = self.task(i).p4.expect("system call from a user task");
        let ok = |addr: u64, len: u64, write: bool| mem.user_range_ok(p4, addr, len, write);
        let fail = |e: i64| Outcome::Ret(-e);
        // Standard output and error go to the program's log, line by line.
        let out = |s: &mut Self, bytes: &[u8]| {
            let t = s.task(i);
            let text = bytes.strip_suffix(b"\n").unwrap_or(bytes);
            log_user(t.pid, &t.name, text);
        };
        match number {
            // write(fd, buf, len)
            1 => {
                if a1 != 1 && a1 != 2 {
                    return fail(EBADF);
                }
                if a3 > 0 && !ok(a2, a3, false) {
                    return fail(EFAULT);
                }
                // SAFETY: just checked as readable user memory of the
                // running process.
                let bytes = unsafe { core::slice::from_raw_parts(a2 as *const u8, a3 as usize) };
                out(self, bytes);
                Outcome::Ret(a3 as i64)
            }
            // writev(fd, iov, count)
            20 => {
                if a1 != 1 && a1 != 2 {
                    return fail(EBADF);
                }
                if a3 > 1024 || !ok(a2, a3 * 16, false) {
                    return fail(EINVAL);
                }
                // One writev is one write: musl sends a line's text and its
                // newline as separate pieces.
                let mut all = alloc::vec::Vec::new();
                for k in 0..a3 {
                    // SAFETY: inside the iovec array checked above.
                    let (base, len) = unsafe {
                        let p = (a2 + 16 * k) as *const u64;
                        (p.read_unaligned(), p.add(1).read_unaligned())
                    };
                    if len == 0 {
                        continue;
                    }
                    if len > LINUX_WRITE_MAX || !ok(base, len, false) {
                        return fail(EFAULT);
                    }
                    // SAFETY: checked as readable user memory.
                    let bytes =
                        unsafe { core::slice::from_raw_parts(base as *const u8, len as usize) };
                    all.extend_from_slice(bytes);
                    if all.len() as u64 > LINUX_WRITE_MAX {
                        return fail(EINVAL);
                    }
                }
                out(self, &all);
                Outcome::Ret(all.len() as i64)
            }
            // ioctl: no terminals yet.
            16 => fail(ENOTTY),
            // getpid, gettid
            39 => Outcome::Ret(self.task(i).pid as i64),
            186 => Outcome::Ret(self.task(i).tid as i64),
            // exit, exit_group
            60 | 231 => Outcome::Exit(a1 as u8),
            // arch_prctl(code, addr)
            158 => match a1 {
                // ARCH_SET_FS
                0x1002 if a2 < USER_HALF_END => {
                    self.task(i).fs_base = a2;
                    FsBase::write(VirtAddr::new_truncate(a2));
                    Outcome::Ret(0)
                }
                // ARCH_GET_FS
                0x1003 if ok(a2, 8, true) => {
                    let fs = self.task(i).fs_base;
                    // SAFETY: checked as writable user memory.
                    unsafe { (a2 as *mut u64).write_unaligned(fs) };
                    Outcome::Ret(0)
                }
                _ => fail(EINVAL),
            },
            // set_tid_address
            218 => Outcome::Ret(self.task(i).tid as i64),
            // ---- memory (docs/linux2-design.md) ----
            // brk(new): grows or shrinks one reserved area; returns the
            // break, unchanged when the request cannot be met.
            12 => {
                let (base, cur) = match self.proc(i).linux_brk {
                    Some(b) => b,
                    None => match self.vm_call(i, sys::VM_RESERVE, 0, LINUX_BRK_SIZE, 0, p4, mem) {
                        Outcome::Ret(b) if b > 0 => {
                            self.proc(i).linux_brk = Some((b as u64, b as u64));
                            (b as u64, b as u64)
                        }
                        _ => return Outcome::Ret(0),
                    },
                };
                if a1 < base || a1 > base + LINUX_BRK_SIZE {
                    return Outcome::Ret(cur as i64);
                }
                let (old_end, new_end) = (page_up(cur), page_up(a1));
                let done = if new_end > old_end {
                    self.vm_call(
                        i,
                        sys::VM_COMMIT,
                        old_end,
                        new_end - old_end,
                        LINUX_RW,
                        p4,
                        mem,
                    )
                } else if new_end < old_end {
                    self.vm_call(i, sys::VM_DECOMMIT, new_end, old_end - new_end, 0, p4, mem)
                } else {
                    Outcome::Ret(0)
                };
                if !matches!(done, Outcome::Ret(0)) {
                    return Outcome::Ret(cur as i64);
                }
                self.proc(i).linux_brk = Some((base, a1));
                Outcome::Ret(a1 as i64)
            }
            // mmap(addr, len, prot, flags, fd, offset): anonymous private
            // memory only, at an address the kernel chooses.
            9 => {
                let flags = f.r10;
                if flags & MAP_ANONYMOUS == 0 || flags & MAP_FIXED != 0 || a2 == 0 {
                    return fail(EINVAL);
                }
                let len = page_up(a2);
                let base = match self.vm_call(i, sys::VM_RESERVE, 0, len, 0, p4, mem) {
                    Outcome::Ret(b) if b > 0 => b as u64,
                    _ => return fail(ENOMEM),
                };
                if a3 & LINUX_RW != 0 {
                    let prot = a3 | zoo_abi::prot::READ;
                    if !matches!(
                        self.vm_call(i, sys::VM_COMMIT, base, len, prot, p4, mem),
                        Outcome::Ret(0)
                    ) {
                        let _ = self.vm_call(i, sys::VM_RELEASE, base, 0, 0, p4, mem);
                        return fail(ENOMEM);
                    }
                }
                Outcome::Ret(base as i64)
            }
            // munmap(addr, len): a whole area is released, part of one is
            // decommitted; nothing mapped there is not an error.
            11 => {
                if a1 % 4096 != 0 || a2 == 0 {
                    return fail(EINVAL);
                }
                let len = page_up(a2);
                let whole = self.proc(i).vmas.iter().any(|&(s, l)| s == a1 && l == len);
                if whole {
                    let _ = self.vm_call(i, sys::VM_RELEASE, a1, 0, 0, p4, mem);
                } else {
                    let _ = self.vm_call(i, sys::VM_DECOMMIT, a1, len, 0, p4, mem);
                }
                Outcome::Ret(0)
            }
            // mprotect: readable protections only (PROT_NONE is not kept).
            10 => {
                if a3 == 0 {
                    return Outcome::Ret(0);
                }
                match self.vm_call(
                    i,
                    sys::VM_PROTECT,
                    a1,
                    page_up(a2),
                    a3 | zoo_abi::prot::READ,
                    p4,
                    mem,
                ) {
                    Outcome::Ret(0) => Outcome::Ret(0),
                    _ => fail(ENOMEM),
                }
            }
            // madvise: advice only.
            28 => Outcome::Ret(0),
            // sched_getaffinity(pid, len, mask): one CPU.
            204 => {
                if a2 < 8 {
                    return fail(EINVAL);
                }
                if !ok(a3, 8, true) {
                    return fail(EFAULT);
                }
                // SAFETY: checked as writable user memory.
                unsafe { (a3 as *mut u64).write_unaligned(1) };
                Outcome::Ret(8)
            }
            // ---- files: read-only, from the initrd ----
            // open(path, flags), openat(dirfd, path, flags)
            2 | 257 => {
                let (path, flags) = if number == 2 { (a1, a2) } else { (a2, a3) };
                if flags & 3 != 0 {
                    return fail(EROFS);
                }
                let mut name = [0u8; LINUX_PATH_MAX];
                let Some(n) = user_cstr(&ok, path, &mut name) else {
                    return fail(EFAULT);
                };
                let Some(data) = core::str::from_utf8(&name[..n])
                    .ok()
                    .and_then(initrd_path)
                    .and_then(initrd::find)
                else {
                    return fail(ENOENT);
                };
                let files = &mut self.proc(i).linux_files;
                let slot = match files.iter().position(Option::is_none) {
                    Some(k) => k,
                    None if files.len() < LINUX_FILES_MAX => {
                        files.push(None);
                        files.len() - 1
                    }
                    None => return fail(EMFILE),
                };
                files[slot] = Some((data, 0));
                Outcome::Ret(slot as i64 + 3)
            }
            // close(fd)
            3 => {
                if a1 < 3 {
                    return Outcome::Ret(0);
                }
                match self.proc(i).linux_files.get_mut((a1 - 3) as usize) {
                    Some(slot @ Some(_)) => {
                        *slot = None;
                        Outcome::Ret(0)
                    }
                    _ => fail(EBADF),
                }
            }
            // read(fd, buf, len), readv(fd, iov, count)
            0 | 19 => {
                // The (address, length) pieces to fill.
                let mut pieces = alloc::vec::Vec::new();
                if number == 0 {
                    pieces.push((a2, a3));
                } else {
                    if a3 > 1024 || !ok(a2, a3 * 16, false) {
                        return fail(EINVAL);
                    }
                    for k in 0..a3 {
                        // SAFETY: inside the iovec array checked above.
                        let piece = unsafe {
                            let p = (a2 + 16 * k) as *const u64;
                            (p.read_unaligned(), p.add(1).read_unaligned())
                        };
                        pieces.push(piece);
                    }
                }
                if a1 == 0 {
                    return Outcome::Ret(0); // standard input: at its end
                }
                let Some(Some((data, pos))) = a1
                    .checked_sub(3)
                    .and_then(|k| self.proc(i).linux_files.get(k as usize))
                    .copied()
                else {
                    return fail(EBADF);
                };
                let mut at = pos as usize;
                let mut total = 0usize;
                for (base, len) in pieces {
                    let n = (len as usize).min(data.len().saturating_sub(at));
                    if n == 0 {
                        continue;
                    }
                    if !ok(base, n as u64, true) {
                        return fail(EFAULT);
                    }
                    // SAFETY: checked as writable user memory; `at + n` is
                    // inside the initrd file.
                    unsafe {
                        core::ptr::copy_nonoverlapping(data[at..].as_ptr(), base as *mut u8, n)
                    };
                    at += n;
                    total += n;
                }
                if let Some(Some(file)) = self.proc(i).linux_files.get_mut((a1 - 3) as usize) {
                    file.1 = at as u64;
                }
                Outcome::Ret(total as i64)
            }
            // lseek(fd, offset, whence)
            8 => {
                let Some(Some((data, pos))) = a1
                    .checked_sub(3)
                    .and_then(|k| self.proc(i).linux_files.get(k as usize))
                    .copied()
                else {
                    return fail(EBADF);
                };
                let from = match a3 {
                    0 => 0i64,
                    1 => pos as i64,
                    2 => data.len() as i64,
                    _ => return fail(EINVAL),
                };
                let new = from.saturating_add(a2 as i64);
                if new < 0 {
                    return fail(EINVAL);
                }
                if let Some(Some(file)) = self.proc(i).linux_files.get_mut((a1 - 3) as usize) {
                    file.1 = new as u64;
                }
                Outcome::Ret(new)
            }
            // fstat(fd, st); stat(path, st); newfstatat(dirfd, path, st, flags)
            5 | 4 | 262 => {
                let (fd, path, st) = match number {
                    5 => (Some(a1), 0, a2),
                    4 => (None, a1, a2),
                    _ if f.r10 & AT_EMPTY_PATH != 0 => (Some(a1), 0, a3),
                    _ => (None, a2, a3),
                };
                let size = match (fd, path) {
                    (Some(0..=2), _) => None,
                    (Some(fd), _) => match self.proc(i).linux_files.get((fd - 3) as usize) {
                        Some(Some((data, _))) => Some(data.len() as u64),
                        _ => return fail(EBADF),
                    },
                    (None, path) => {
                        let mut name = [0u8; LINUX_PATH_MAX];
                        let Some(n) = user_cstr(&ok, path, &mut name) else {
                            return fail(EFAULT);
                        };
                        match core::str::from_utf8(&name[..n])
                            .ok()
                            .and_then(initrd_path)
                            .and_then(initrd::find)
                        {
                            Some(data) => Some(data.len() as u64),
                            None => return fail(ENOENT),
                        }
                    }
                };
                if !ok(st, 144, true) {
                    return fail(EFAULT);
                }
                // struct stat (x86-64): mode at 24, size at 48, blksize at
                // 56, blocks at 64; the rest zero. A regular read-only file,
                // or a character device for the standard descriptors.
                let mut buf = [0u8; 144];
                let (mode, len) = match size {
                    Some(len) => (0o100_444u32, len),
                    None => (0o020_620u32, 0),
                };
                buf[16..24].copy_from_slice(&1u64.to_le_bytes());
                buf[24..28].copy_from_slice(&mode.to_le_bytes());
                buf[48..56].copy_from_slice(&len.to_le_bytes());
                buf[56..64].copy_from_slice(&4096u64.to_le_bytes());
                buf[64..72].copy_from_slice(&len.div_ceil(512).to_le_bytes());
                // SAFETY: checked as writable user memory.
                unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), st as *mut u8, 144) };
                Outcome::Ret(0)
            }
            // ---- time ----
            // clock_gettime(clock, ts)
            228 => {
                let ns = match a1 {
                    0 | 8 => match rtc::realtime_ns() {
                        Some(ns) => ns,
                        None => return fail(EINVAL),
                    },
                    1 | 4 | 7 | 9 => rtc::monotonic_ns(),
                    _ => return fail(EINVAL),
                };
                if !ok(a2, 16, true) {
                    return fail(EFAULT);
                }
                // SAFETY: checked as writable user memory.
                unsafe {
                    let p = a2 as *mut u64;
                    p.write_unaligned(ns / 1_000_000_000);
                    p.add(1).write_unaligned(ns % 1_000_000_000);
                }
                Outcome::Ret(0)
            }
            // nanosleep(req, rem), clock_nanosleep(clock, flags, req, rem):
            // relative sleeps, rounded up to timer ticks.
            35 | 230 => {
                let req = if number == 35 { a1 } else { a3 };
                if number == 230 && a2 & 1 != 0 {
                    return fail(EINVAL); // TIMER_ABSTIME: not yet
                }
                if !ok(req, 16, false) {
                    return fail(EFAULT);
                }
                // SAFETY: checked as readable user memory.
                let (sec, nsec) = unsafe {
                    let p = req as *const u64;
                    (p.read_unaligned(), p.add(1).read_unaligned())
                };
                if nsec >= 1_000_000_000 {
                    return fail(EINVAL);
                }
                let tick_ns = 1_000_000_000 / u64::from(interrupts::TIMER_HZ);
                let ticks = sec
                    .saturating_mul(u64::from(interrupts::TIMER_HZ))
                    .saturating_add(nsec.div_ceil(tick_ns));
                let now = interrupts::ticks();
                let t = self.task(i);
                let deadline = *t.sleep_until.get_or_insert(now.saturating_add(ticks));
                if now >= deadline {
                    t.sleep_until = None;
                    Outcome::Ret(0)
                } else {
                    Outcome::Block
                }
            }
            n => {
                let (word, bit) = ((n / 64) as usize, n % 64);
                if word < LINUX_REPORTED.len()
                    && LINUX_REPORTED[word].fetch_or(1 << bit, Ordering::Relaxed) & (1 << bit) == 0
                {
                    kprintln!("ZOO_OS INFO linux unsupported syscall nr={}", n);
                }
                fail(ENOSYS)
            }
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
                let t = self.proc(i);
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
                if a2 == 0 || a2 > MAX_CMDLINE as u64 {
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
                let t = self.proc(i);
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
                match self.spawn(mem, &name, [give, None]) {
                    Ok((_, pid)) => {
                        let t = self.proc(i);
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
                let (chan, side) = match self.proc(i).get(a1) {
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
                    c.queues[peer].push_back(Msg::Bytes(msg.to_vec()));
                }
                self.wake_all();
                Outcome::Ret(0)
            }
            sys::RECV | sys::TRY_RECV => {
                let (chan, side) = match self.proc(i).get(a1) {
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
                let len = match c.queues[side].front() {
                    None => {
                        return if !c.open[1 - side] {
                            err(Error::PeerClosed)
                        } else if number == sys::TRY_RECV {
                            err(Error::Again)
                        } else {
                            Outcome::Block
                        };
                    }
                    Some(Msg::Handle(..)) => return err(Error::WrongKind),
                    Some(Msg::Bytes(b)) => b.len(),
                };
                if len as u64 > a3 {
                    return err(Error::TooSmall);
                }
                let Some(Msg::Bytes(msg)) = c.queues[side].pop_front() else {
                    unreachable!("front is a bytes message");
                };
                // SAFETY: checked writable user range of `a3 >= len` bytes in
                // the active space.
                unsafe { core::ptr::copy_nonoverlapping(msg.as_ptr(), a2 as *mut u8, len) };
                Outcome::Ret(len as i64)
            }
            sys::WAIT => {
                let pid = match self.proc(i).get(a1) {
                    None => return err(Error::BadHandle),
                    Some(HandleEntry {
                        object: Object::Thread { tid },
                        rights,
                    }) if rights & RIGHT_WAIT != 0 => {
                        let tid = *tid;
                        return match self.thread_exits.iter().find(|e| e.1 == tid) {
                            Some(&(_, _, code)) => Outcome::Ret(code as i64),
                            None => Outcome::Block,
                        };
                    }
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
                let pid = match self.proc(i).get(a1) {
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
                    // Killing your own process is an exit (all its threads).
                    Some(_) if pid == by => Outcome::Exit(0),
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
                if self.proc(i).display_pages == 0 {
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
                    self.proc(i).display_pages = pages;
                }
                self.display_owner = Some(me);
                DISPLAY_OWNED.store(true, Ordering::Relaxed);
                // Movement made before this owner arrived is not its own.
                mouse::clear();
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
            sys::MEM_ALLOC => {
                if a1 == 0 || a1 > HEAP_LIMIT {
                    return err(Error::Inval);
                }
                let pages = a1.div_ceil(4096);
                let t = self.proc(i);
                let start = t.heap_next;
                if start - HEAP_BASE + pages * 4096 > HEAP_LIMIT {
                    return err(Error::NoMem);
                }
                for k in 0..pages {
                    if mem
                        .map_user_page(p4, start + k * 4096, true, false)
                        .is_err()
                    {
                        mem.unmap_user_free(p4, start, k);
                        return err(Error::NoMem);
                    }
                }
                self.proc(i).heap_next = start + pages * 4096;
                Outcome::Ret(start as i64)
            }
            sys::FS_MAP => {
                let size = core::mem::size_of::<MapInfo>() as u64;
                if !ok(a2, size, true) {
                    return err(Error::Fault);
                }
                let (Some((_, len, offset)), Some(base)) =
                    (initrd::entry(a1 as usize), initrd::base())
                else {
                    return err(Error::NotFound);
                };
                let data = base + offset as u64;
                let first = data & !0xFFF;
                let end = (data + len as u64).div_ceil(4096) * 4096;
                let pages = (end - first) / 4096;
                let va = self.proc(i).map_next;
                for k in 0..pages {
                    let phys = mem.mapper.translate_addr(VirtAddr::new(first + k * 4096));
                    let mapped = phys.map(|p| {
                        mem.map_user_frame(
                            p4,
                            va + k * 4096,
                            PhysFrame::containing_address(p),
                            false,
                        )
                    });
                    if !matches!(mapped, Some(Ok(()))) {
                        mem.unmap_user_keep(p4, va, k);
                        return err(Error::NoMem);
                    }
                }
                let t = self.proc(i);
                t.file_maps.push((va, pages));
                // Leave an unmapped page between mappings.
                t.map_next = va + (pages + 1) * 4096;
                let info = MapInfo {
                    addr: va + (data & 0xFFF),
                    len: len as u64,
                };
                // SAFETY: checked writable user range of `size` bytes.
                unsafe { (a2 as *mut MapInfo).write_unaligned(info) };
                Outcome::Ret(0)
            }
            sys::THREAD_CREATE => {
                // a1 = entry, a2 = stack top (16-byte aligned), a3 = argument.
                if a1 >= USER_HALF_END || a2 >= USER_HALF_END || a2 % 16 != 0 || a2 == 0 {
                    return err(Error::Inval);
                }
                if self.proc(i).free_slots() < 1 {
                    return err(Error::NoMem);
                }
                let (index, slot) = match self.free_task_slots() {
                    Ok(x) => x,
                    Err(e) => return err(e),
                };
                let Ok(top) = mem.map_kernel_stack(slot) else {
                    return err(Error::NoMem);
                };
                self.kstack_used[slot] = true;
                let tid = self.next_tid;
                self.next_tid += 1;
                let (pid, pidx, name) = {
                    let t = self.task(i);
                    (t.pid, t.proc, t.name.clone())
                };
                let mut task = Task::new(pid, tid, &name);
                // SysV: at function entry, rsp + 8 is 16-byte aligned.
                task.frame = Self::first_frame(top, a1, a2 - 8, a3, 0);
                task.fpu = Some(Box::new(fpu::new_state()));
                task.p4 = Some(p4);
                task.proc = pidx;
                task.kstack = Some((slot, top));
                self.tasks[index] = Some(task);
                let p = self.proc(i);
                p.live += 1;
                p.members += 1;
                let h = p
                    .insert(HandleEntry {
                        object: Object::Thread { tid },
                        rights: RIGHT_WAIT,
                    })
                    .expect("checked a free slot");
                kprintln!("ZOO_OS INFO thread start pid={} tid={}", pid, tid);
                Outcome::Ret(h as i64)
            }
            sys::THREAD_EXIT if a1 > 255 => err(Error::Inval),
            sys::THREAD_EXIT => Outcome::ThreadExit(a1 as u8),
            sys::SET_FS_BASE => {
                if a1 >= USER_HALF_END {
                    return err(Error::Inval);
                }
                self.task(i).fs_base = a1;
                FsBase::write(VirtAddr::new_truncate(a1));
                Outcome::Ret(0)
            }
            sys::FUTEX_WAIT => {
                // a1 = address of a u32, a2 = expected value, a3 = timeout in
                // ticks (0: none). Returns 0 when woken or the value differs.
                if a1 % 4 != 0 || !ok(a1, 4, false) {
                    return err(Error::Fault);
                }
                let now = interrupts::ticks();
                let t = self.task(i);
                if t.futex_woken {
                    t.futex_woken = false;
                    t.futex = None;
                    t.sleep_until = None;
                    return Outcome::Ret(0);
                }
                // SAFETY: four readable, aligned user bytes in the active
                // address space (checked above); other threads may write it,
                // hence a volatile read.
                let value = unsafe { core::ptr::read_volatile(a1 as *const u32) };
                if value as u64 != a2 & 0xFFFF_FFFF {
                    t.futex = None;
                    t.sleep_until = None;
                    return Outcome::Ret(0);
                }
                if a3 > 0 {
                    let deadline = *t.sleep_until.get_or_insert(now.saturating_add(a3));
                    if now >= deadline {
                        t.futex = None;
                        t.sleep_until = None;
                        return err(Error::TimedOut);
                    }
                }
                t.futex = Some(a1);
                Outcome::Block
            }
            sys::FUTEX_WAKE => {
                let pidx = self.task(i).proc;
                let mut woken = 0u64;
                for t in self.tasks.iter_mut().flatten() {
                    if woken == a2 {
                        break;
                    }
                    // A waiter may already be Ready (restarted by a general
                    // wake-up) but not yet re-run; it still counts.
                    if t.proc == pidx
                        && t.futex == Some(a1)
                        && !t.futex_woken
                        && t.state != State::Exited
                    {
                        t.futex_woken = true;
                        t.state = State::Ready;
                        woken += 1;
                    }
                }
                Outcome::Ret(woken as i64)
            }
            sys::VM_RESERVE
            | sys::VM_COMMIT
            | sys::VM_PROTECT
            | sys::VM_DECOMMIT
            | sys::VM_RELEASE
            | sys::VM_QUERY => self.vm_call(i, number, a1, a2, a3, p4, mem),
            sys::PROCESS_ID => {
                if a1 == NO_HANDLE {
                    return Outcome::Ret(self.task(i).pid as i64);
                }
                match self.proc(i).get(a1) {
                    Some(HandleEntry {
                        object: Object::Process { pid },
                        ..
                    }) => Outcome::Ret(*pid as i64),
                    Some(_) => err(Error::Inval),
                    None => err(Error::BadHandle),
                }
            }
            sys::SEND_HANDLE => {
                // a1 = channel, a2 = handle to move, a3 = tag.
                if a1 == a2 {
                    return err(Error::Inval); // an end cannot carry itself
                }
                let (chan, side) = match self.proc(i).get(a1) {
                    None => return err(Error::BadHandle),
                    Some(HandleEntry {
                        object: Object::Endpoint { chan, side },
                        rights,
                    }) if rights & RIGHT_SEND != 0 => (chan.clone(), *side),
                    Some(_) => return err(Error::Rights),
                };
                if self.proc(i).get(a2).is_none() {
                    return err(Error::BadHandle);
                }
                {
                    let c = chan.lock();
                    let peer = 1 - side;
                    if !c.open[peer] {
                        return err(Error::PeerClosed);
                    }
                    if c.queues[peer].len() >= CHANNEL_DEPTH {
                        return err(Error::Full);
                    }
                }
                // Moved: it leaves the sender's table (its old value goes
                // stale) and travels in the queue.
                let entry = self.proc(i).take(a2).expect("checked above");
                chan.lock().queues[1 - side].push_back(Msg::Handle(entry, a3));
                self.wake_all();
                Outcome::Ret(0)
            }
            sys::RECV_HANDLE => {
                // a1 = channel, a2 = out `HandleMsg`, a3 = flags.
                let (chan, side) = match self.proc(i).get(a1) {
                    None => return err(Error::BadHandle),
                    Some(HandleEntry {
                        object: Object::Endpoint { chan, side },
                        rights,
                    }) if rights & RIGHT_RECV != 0 => (chan.clone(), *side),
                    Some(_) => return err(Error::Rights),
                };
                let size = core::mem::size_of::<HandleMsg>() as u64;
                if !ok(a2, size, true) {
                    return err(Error::Fault);
                }
                if self.proc(i).free_slots() == 0 {
                    return err(Error::NoMem);
                }
                let taken = {
                    let mut c = chan.lock();
                    match c.queues[side].front() {
                        None => Err(if !c.open[1 - side] {
                            err(Error::PeerClosed)
                        } else if a3 & RECV_NONBLOCK != 0 {
                            err(Error::Again)
                        } else {
                            Outcome::Block
                        }),
                        Some(Msg::Bytes(_)) => Err(err(Error::WrongKind)),
                        Some(Msg::Handle(..)) => match c.queues[side].pop_front() {
                            Some(Msg::Handle(e, tag)) => Ok((e, tag)),
                            _ => unreachable!("front is a handle message"),
                        },
                    }
                };
                let (entry, tag) = match taken {
                    Ok(t) => t,
                    Err(outcome) => return outcome,
                };
                let h = self.proc(i).insert(entry).expect("checked a free slot");
                // SAFETY: checked writable user range of the struct's size.
                unsafe { (a2 as *mut HandleMsg).write_unaligned(HandleMsg { handle: h, tag }) };
                self.wake_all();
                Outcome::Ret(0)
            }
            sys::WAIT_ANY => {
                // a1 = handle array, a2 = count, a3 = timeout in ticks
                // (`WAIT_FOREVER` for none). Returns the index of the first
                // ready handle: a channel end with a message waiting or a
                // closed peer, or an ended process or thread. `TIMEDOUT`
                // after the timeout (0 polls once).
                if a2 == 0 || a2 > WAIT_ANY_MAX as u64 {
                    return err(Error::Inval);
                }
                if !ok(a1, a2 * 8, false) {
                    return err(Error::Fault);
                }
                let mut ready = None;
                for k in 0..a2 {
                    // SAFETY: inside the checked readable user range.
                    let h = unsafe { ((a1 + k * 8) as *const u64).read_unaligned() };
                    let is_ready = match self.proc(i).get(h) {
                        None => return err(Error::BadHandle),
                        Some(HandleEntry {
                            object: Object::Endpoint { chan, side },
                            ..
                        }) => {
                            let c = chan.lock();
                            !c.queues[*side].is_empty() || !c.open[1 - *side]
                        }
                        Some(HandleEntry {
                            object: Object::Process { pid },
                            ..
                        }) => {
                            let pid = *pid;
                            self.exits.iter().any(|(p, _)| *p == pid)
                        }
                        Some(HandleEntry {
                            object: Object::Thread { tid },
                            ..
                        }) => {
                            let tid = *tid;
                            self.thread_exits.iter().any(|e| e.1 == tid)
                        }
                        Some(_) => false,
                    };
                    if is_ready {
                        ready = Some(k);
                        break;
                    }
                }
                let now = interrupts::ticks();
                let t = self.task(i);
                if let Some(k) = ready {
                    t.sleep_until = None;
                    return Outcome::Ret(k as i64);
                }
                if a3 != WAIT_FOREVER {
                    let deadline = *t.sleep_until.get_or_insert(now.saturating_add(a3));
                    if now >= deadline {
                        t.sleep_until = None;
                        return err(Error::TimedOut);
                    }
                }
                // Woken by any send, close or exit (wake_all) or the timer.
                Outcome::Block
            }
            sys::ARGS_GET => {
                // a1 = buffer, a2 = its length; returns the full length.
                let cmdline = self.proc(i).cmdline.as_bytes();
                let n = cmdline.len().min(a2 as usize);
                if n > 0 {
                    if !ok(a1, n as u64, true) {
                        return err(Error::Fault);
                    }
                    // SAFETY: checked writable user range in the active space.
                    unsafe { core::ptr::copy_nonoverlapping(cmdline.as_ptr(), a1 as *mut u8, n) };
                }
                Outcome::Ret(cmdline.len() as i64)
            }
            sys::CLOCK => match a1 {
                CLOCK_REALTIME => match rtc::realtime_ns() {
                    Some(ns) => Outcome::Ret(ns as i64),
                    None => err(Error::NotFound),
                },
                CLOCK_MONOTONIC => Outcome::Ret(rtc::monotonic_ns() as i64),
                _ => err(Error::Inval),
            },
            sys::MOUSE_READ => {
                // a1 = buffer of `MouseEvent`s, a2 = how many fit.
                let me = self.task(i).pid;
                if self.display_owner != Some(me) {
                    return err(Error::Rights);
                }
                let size = core::mem::size_of::<MouseEvent>() as u64;
                if a2 == 0 || !ok(a1, a2 * size, true) {
                    return err(Error::Fault);
                }
                let mut n = 0u64;
                while n < a2 {
                    let Some((dx, dy, buttons)) = mouse::pop() else {
                        break;
                    };
                    let e = MouseEvent {
                        dx,
                        dy,
                        buttons,
                        _pad: [0; 3],
                    };
                    // SAFETY: inside the writable user range checked above.
                    unsafe { ((a1 + n * size) as *mut MouseEvent).write_unaligned(e) };
                    n += 1;
                }
                Outcome::Ret(n as i64)
            }
            sys::BLOCK_ATTACH => {
                let p = self.proc(i);
                if !p.log_ok {
                    return err(Error::Rights);
                }
                if virtio_blk::capacity().is_none() {
                    return err(Error::NotFound);
                }
                match p.insert(HandleEntry {
                    object: Object::Block,
                    rights: RIGHT_SEND | RIGHT_RECV,
                }) {
                    Some(h) => Outcome::Ret(h as i64),
                    None => err(Error::NoMem),
                }
            }
            sys::BLOCK_READ | sys::BLOCK_WRITE | sys::BLOCK_SIZE => {
                // a1 = block handle, a2 = block number, a3 = 4 KiB buffer.
                match self.proc(i).get(a1) {
                    Some(HandleEntry {
                        object: Object::Block,
                        ..
                    }) => {}
                    Some(_) => return err(Error::Rights),
                    None => return err(Error::BadHandle),
                }
                if number == sys::BLOCK_SIZE {
                    return Outcome::Ret(virtio_blk::capacity().unwrap_or(0) as i64);
                }
                let write = number == sys::BLOCK_WRITE;
                if !ok(a3, BLOCK_BYTES as u64, !write) {
                    return err(Error::Fault);
                }
                // SAFETY: a checked, mapped user range of exactly one block
                // in the active address space (writable when reading into
                // it); `[u8; N]` has alignment 1.
                let buf = unsafe { &mut *(a3 as *mut [u8; BLOCK_BYTES]) };
                match virtio_blk::transfer(mem, a2, buf, write) {
                    Ok(()) => Outcome::Ret(0),
                    Err(virtio_blk::BlockError::OutOfRange) => err(Error::Inval),
                    Err(virtio_blk::BlockError::NoDevice) => err(Error::NotFound),
                    Err(_) => err(Error::Fault),
                }
            }
            sys::LOG_ATTACH => {
                let p = self.proc(i);
                if !p.log_ok {
                    return err(Error::Rights);
                }
                match p.insert(HandleEntry {
                    object: Object::LogReader,
                    rights: RIGHT_RECV,
                }) {
                    Some(h) => Outcome::Ret(h as i64),
                    None => err(Error::NoMem),
                }
            }
            sys::LOG_READ => {
                // a1 = log-reader handle, a2/a3 = buffer. Returns the bytes of
                // one line, `AGAIN` when the buffer is empty.
                match self.proc(i).get(a1) {
                    Some(HandleEntry {
                        object: Object::LogReader,
                        ..
                    }) => {}
                    Some(_) => return err(Error::Rights),
                    None => return err(Error::BadHandle),
                }
                if !ok(a2, a3, true) {
                    return err(Error::Fault);
                }
                let Some(line) = klog::pop() else {
                    return err(Error::Again);
                };
                let n = line.len().min(a3 as usize);
                // SAFETY: `n` bytes fit the checked writable user range.
                unsafe { core::ptr::copy_nonoverlapping(line.as_ptr(), a2 as *mut u8, n) };
                Outcome::Ret(n as i64)
            }
            sys::SET_EXCEPTION_HANDLER => {
                if a1 >= USER_HALF_END {
                    return err(Error::Inval);
                }
                self.proc(i).exc_handler = a1;
                Outcome::Ret(0)
            }
            sys::SET_EXCEPTION_STACK => {
                if a1 >= USER_HALF_END || !a1.is_multiple_of(16) {
                    return err(Error::Inval);
                }
                self.task(i).exc_stack = a1;
                Outcome::Ret(0)
            }
            sys::EXCEPTION_RETURN => self.exception_return(i, f, a1, a2, p4, mem),
            sys::SPAWN_WITH => {
                if a2 == 0 || a2 > MAX_CMDLINE as u64 {
                    return err(Error::Inval);
                }
                if !ok(a1, a2, false) || !ok(a3, 16, false) {
                    return err(Error::Fault);
                }
                // SAFETY: checked readable user ranges in the active space;
                // unaligned reads because user pointers need not be aligned.
                let (raw, values) = unsafe {
                    (
                        core::slice::from_raw_parts(a1 as *const u8, a2 as usize),
                        [
                            (a3 as *const u64).read_unaligned(),
                            ((a3 + 8) as *const u64).read_unaligned(),
                        ],
                    )
                };
                let Ok(name) = core::str::from_utf8(raw) else {
                    return err(Error::Inval);
                };
                let name = String::from(name);
                if values[0] != NO_HANDLE && values[0] == values[1] {
                    return err(Error::Inval);
                }
                let t = self.proc(i);
                let mut give = [None, None];
                for (g, &v) in give.iter_mut().zip(&values) {
                    if v != NO_HANDLE {
                        match t.get(v) {
                            Some(h) => *g = Some(h.clone()),
                            None => return err(Error::BadHandle),
                        }
                    }
                }
                // Moving a handle frees its slot, so the process handle
                // always fits when any were given.
                if t.free_slots() < 1 && give.iter().all(Option::is_none) {
                    return err(Error::NoMem);
                }
                match self.spawn(mem, &name, give) {
                    Ok((_, pid)) => {
                        let t = self.proc(i);
                        for &v in &values {
                            if v != NO_HANDLE {
                                // Moved into the child: removed, not released.
                                let _ = t.take(v);
                            }
                        }
                        let h = t
                            .insert(HandleEntry {
                                object: Object::Process { pid },
                                rights: RIGHT_WAIT | RIGHT_KILL,
                            })
                            .expect("a slot was free or freed");
                        Outcome::Ret(h as i64)
                    }
                    Err(e) => err(e),
                }
            }
            sys::CLOSE => match self.proc(i).take(a1) {
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

    /// `exception_return(ctx, how)`: resumes the interrupted thread from the
    /// (possibly edited) context, or crashes the process (docs/posix3-design.md).
    fn exception_return(
        &mut self,
        i: usize,
        f: &mut TrapFrame,
        a1: u64,
        a2: u64,
        p4: PhysFrame,
        mem: &mut Memory,
    ) -> Outcome {
        if !self.task(i).in_exception {
            return err(Error::Inval);
        }
        let size = core::mem::size_of::<ExceptionContext>() as u64;
        if !mem.user_range_ok(p4, a1, size, false) {
            return err(Error::Fault);
        }
        // SAFETY: checked readable user range of the struct's size; copied
        // into the kernel before anything is checked.
        let ctx = unsafe { (a1 as *const ExceptionContext).read_unaligned() };
        if a2 == EXC_UNHANDLED {
            let t = self.task(i);
            t.in_exception = false;
            let line = crash_line(t.pid, &t.name, ctx.vector, ctx.error, ctx.addr, ctx.rip);
            kprintln!("{}", line);
            kprintln!("ZOO_OS INFO crash declared unhandled by the program");
            return Outcome::Crash;
        }
        if a2 != EXC_RESUME
            || ctx.rip >= USER_HALF_END
            || ctx.rsp >= USER_HALF_END
            || ctx.fs_base >= USER_HALF_END
        {
            return err(Error::Inval);
        }
        let Some(fx) = fpu::from_user(&ctx.fx) else {
            return err(Error::Inval);
        };
        // Only arithmetic and direction flags come from the program;
        // interrupts stay on and privileged bits stay clear.
        const USER_FLAGS: u64 = 0xCD5; // CF PF AF ZF SF DF OF
        f.rflags = (ctx.rflags & USER_FLAGS) | RFLAGS_IF | RFLAGS_RESERVED;
        f.rip = ctx.rip;
        f.rsp = ctx.rsp;
        f.rax = ctx.rax;
        f.rbx = ctx.rbx;
        f.rcx = ctx.rcx;
        f.rdx = ctx.rdx;
        f.rsi = ctx.rsi;
        f.rdi = ctx.rdi;
        f.rbp = ctx.rbp;
        f.r8 = ctx.r8;
        f.r9 = ctx.r9;
        f.r10 = ctx.r10;
        f.r11 = ctx.r11;
        f.r12 = ctx.r12;
        f.r13 = ctx.r13;
        f.r14 = ctx.r14;
        f.r15 = ctx.r15;
        fpu::restore(&fx);
        let t = self.task(i);
        t.fs_base = ctx.fs_base;
        FsBase::write(VirtAddr::new_truncate(ctx.fs_base));
        t.in_exception = false;
        Outcome::Resume
    }

    /// Delivers a user fault to the process's handler: writes the context
    /// on the exception stack and redirects the thread. Returns false when
    /// there is no handler, the thread is already handling one, or the
    /// context does not fit; the caller then crashes the process.
    fn deliver_exception(
        &mut self,
        i: usize,
        f: &mut TrapFrame,
        addr: u64,
        mem: &mut Memory,
    ) -> bool {
        let handler = self.proc(i).exc_handler;
        let t = self.task(i);
        if handler == 0 || t.in_exception {
            return false;
        }
        let Some(p4) = t.p4 else { return false };
        let size = core::mem::size_of::<ExceptionContext>() as u64;
        // Below the red zone of the interrupted stack unless the thread set
        // its own exception stack.
        let top = if t.exc_stack != 0 {
            t.exc_stack
        } else {
            f.rsp.wrapping_sub(128)
        };
        let ctx_addr = top.wrapping_sub(size) & !15;
        if top >= USER_HALF_END || ctx_addr > top || !mem.user_range_ok(p4, ctx_addr, size, true) {
            return false;
        }
        let mut fx = fpu::new_state();
        fpu::save(&mut fx);
        let ctx = ExceptionContext {
            vector: f.vector,
            error: f.error,
            addr,
            rip: f.rip,
            rsp: f.rsp,
            rflags: f.rflags,
            rax: f.rax,
            rbx: f.rbx,
            rcx: f.rcx,
            rdx: f.rdx,
            rsi: f.rsi,
            rdi: f.rdi,
            rbp: f.rbp,
            r8: f.r8,
            r9: f.r9,
            r10: f.r10,
            r11: f.r11,
            r12: f.r12,
            r13: f.r13,
            r14: f.r14,
            r15: f.r15,
            fs_base: t.fs_base,
            fx: *fpu::bytes(&fx),
        };
        // SAFETY: checked writable user range of the struct's size in the
        // active address space; unaligned write for safety although the
        // address is 16-byte aligned.
        unsafe { (ctx_addr as *mut ExceptionContext).write_unaligned(ctx) };
        t.in_exception = true;
        kprintln!(
            "ZOO_OS INFO exception pid={} tid={} vector={} addr={:#x} delivered",
            t.pid,
            t.tid,
            f.vector,
            addr
        );
        // SysV: at function entry, rsp + 8 is 16-byte aligned.
        f.rsp = ctx_addr - 8;
        f.rip = handler;
        f.rdi = ctx_addr;
        f.rflags = RFLAGS_IF | RFLAGS_RESERVED;
        true
    }

    /// System calls 32-37: reserve, commit, protect, decommit, release and
    /// query address ranges in the `vm` window (docs/posix2-design.md).
    #[allow(clippy::too_many_arguments)]
    fn vm_call(
        &mut self,
        i: usize,
        number: u64,
        a1: u64,
        a2: u64,
        a3: u64,
        p4: PhysFrame,
        mem: &mut Memory,
    ) -> Outcome {
        const PAGE: u64 = 4096;
        let in_window = |a: u64, len: u64| {
            a >= VM_START && len > 0 && a.checked_add(len).is_some_and(|e| e <= VM_END)
        };
        // The reservation containing [a, a+len), if any.
        let find = |vmas: &[(u64, u64)], a: u64, len: u64| {
            vmas.iter().position(|&(s, l)| a >= s && a + len <= s + l)
        };
        let access = |prot: u64| -> Option<(bool, bool)> {
            if prot & !(prot::READ | prot::WRITE | prot::EXEC) != 0 || prot & prot::READ == 0 {
                return None;
            }
            Some((prot & prot::WRITE != 0, prot & prot::EXEC != 0))
        };
        match number {
            sys::VM_RESERVE => {
                // a1 = address (0: kernel chooses), a2 = length.
                if !a1.is_multiple_of(PAGE) || !a2.is_multiple_of(PAGE) || a2 == 0 {
                    return err(Error::Inval);
                }
                let p = self.proc(i);
                if p.vmas.len() >= VM_MAX_RESERVATIONS {
                    return err(Error::NoMem);
                }
                let base = if a1 == 0 {
                    // First fit, granule-aligned, between existing ones.
                    let mut cand = VM_START;
                    for &(s, l) in &p.vmas {
                        if cand + a2 <= s {
                            break;
                        }
                        cand = (s + l).div_ceil(VM_GRANULE) * VM_GRANULE;
                    }
                    if !in_window(cand, a2) {
                        return err(Error::NoMem);
                    }
                    cand
                } else {
                    if !in_window(a1, a2) {
                        return err(Error::Inval);
                    }
                    if p.vmas.iter().any(|&(s, l)| a1 < s + l && s < a1 + a2) {
                        return err(Error::Exists);
                    }
                    a1
                };
                let at = p.vmas.partition_point(|&(s, _)| s < base);
                p.vmas.insert(at, (base, a2));
                Outcome::Ret(base as i64)
            }
            sys::VM_COMMIT | sys::VM_PROTECT => {
                // a1 = address, a2 = length, a3 = protection.
                if !a1.is_multiple_of(PAGE) || !a2.is_multiple_of(PAGE) || !in_window(a1, a2) {
                    return err(Error::Inval);
                }
                let Some((w, x)) = access(a3) else {
                    return err(Error::Inval);
                };
                if find(&self.proc(i).vmas, a1, a2).is_none() {
                    return err(Error::NotFound);
                }
                let end = a1 + a2;
                let pages = a2 / PAGE;
                let mut mapped = 0u64;
                let mut va = a1;
                while let Some(m) = mem.next_mapped(p4, va, end) {
                    mapped += 1;
                    va = m + PAGE;
                }
                if number == sys::VM_PROTECT {
                    if mapped != pages {
                        return err(Error::NotFound); // some page is not committed
                    }
                } else {
                    let new = pages - mapped;
                    let p = self.proc(i);
                    if (p.committed + new) * PAGE > VM_COMMIT_LIMIT
                        || (mem.frames.available() as u64) < new + new / 256 + 64
                    {
                        return err(Error::NoMem);
                    }
                }
                let mut va = a1;
                while va < end {
                    let r = if mem.user_translate(p4, va).is_some() {
                        mem.protect_user_page(p4, va, w, x)
                    } else {
                        let r = mem.map_user_page(p4, va, w, x).map(|_| ());
                        if r.is_ok() {
                            self.proc(i).committed += 1;
                        }
                        r
                    };
                    if r.is_err() {
                        return err(Error::NoMem);
                    }
                    va += PAGE;
                }
                Outcome::Ret(0)
            }
            sys::VM_DECOMMIT | sys::VM_RELEASE => {
                let (start, end, remove) = if number == sys::VM_RELEASE {
                    // a1 = the reservation's base.
                    match self.proc(i).vmas.iter().position(|&(s, _)| s == a1) {
                        Some(k) => {
                            let (s, l) = self.proc(i).vmas[k];
                            (s, s + l, Some(k))
                        }
                        None => return err(Error::NotFound),
                    }
                } else {
                    if !a1.is_multiple_of(PAGE) || !a2.is_multiple_of(PAGE) || !in_window(a1, a2) {
                        return err(Error::Inval);
                    }
                    if find(&self.proc(i).vmas, a1, a2).is_none() {
                        return err(Error::NotFound);
                    }
                    (a1, a1 + a2, None)
                };
                let mut va = start;
                while let Some(m) = mem.next_mapped(p4, va, end) {
                    mem.unmap_user_free(p4, m, 1);
                    self.proc(i).committed -= 1;
                    va = m + PAGE;
                }
                if let Some(k) = remove {
                    self.proc(i).vmas.remove(k);
                }
                Outcome::Ret(0)
            }
            _ => {
                // VM_QUERY: a1 = address, a2 = out pointer.
                let size = core::mem::size_of::<VmInfo>() as u64;
                if !mem.user_range_ok(p4, a2, size, true) {
                    return err(Error::Fault);
                }
                let a = a1 & !(PAGE - 1);
                if !in_window(a, PAGE) {
                    return err(Error::Inval);
                }
                let vmas = &self.proc(i).vmas;
                let info = match vmas.iter().find(|&&(s, l)| a >= s && a < s + l) {
                    None => {
                        let next = vmas
                            .iter()
                            .map(|&(s, _)| s)
                            .find(|&s| s > a)
                            .unwrap_or(VM_END);
                        VmInfo {
                            base: a,
                            len: next - a,
                            state: VM_FREE,
                            ..VmInfo::default()
                        }
                    }
                    Some(&(s, l)) => {
                        let end = s + l;
                        let (state, prot, run_end) = match mem.user_page_access(p4, a) {
                            None => {
                                let next = mem.next_mapped(p4, a, end).unwrap_or(end);
                                (VM_RESERVED, 0, next)
                            }
                            Some(acc) => {
                                let mut e = a + PAGE;
                                while e < end && mem.user_page_access(p4, e) == Some(acc) {
                                    e += PAGE;
                                }
                                let prot = prot::READ
                                    | if acc.0 { prot::WRITE } else { 0 }
                                    | if acc.1 { prot::EXEC } else { 0 };
                                (VM_COMMITTED, prot, e)
                            }
                        };
                        VmInfo {
                            base: a,
                            len: run_end - a,
                            state,
                            prot,
                            reservation: s,
                            reservation_len: l,
                        }
                    }
                };
                // SAFETY: checked writable user range of the struct's size.
                unsafe { (a2 as *mut VmInfo).write_unaligned(info) };
                Outcome::Ret(0)
            }
        }
    }

    /// Saves the current frame and switches to the next ready task.
    fn switch_away(&mut self, frame: *mut TrapFrame, mem: &mut Memory) -> *mut TrapFrame {
        let cur = self.current;
        let t = self.task(cur);
        t.frame = frame as u64;
        if let Some(area) = &mut t.fpu {
            fpu::save(area);
        }
        let next = self.pick_next();
        if next == cur && self.task(cur).state == State::Ready {
            return frame;
        }
        self.activate(next, mem)
    }
}

/// Copies each segment's file bytes into freshly mapped user pages.
/// Maps and fills the program's segments and its stack; returns the frame
/// of the stack's top page.
fn load_image(
    mem: &mut Memory,
    p4: PhysFrame,
    file: &[u8],
    image: &elf::Image,
) -> Result<PhysFrame, ()> {
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
    let mut top = None;
    while page < USER_STACK_TOP {
        top = Some(mem.map_user_page(p4, page, true, false).map_err(|_| ())?);
        page += 4096;
    }
    top.ok_or(())
}

// ---- Linux programs (docs/linux1-design.md) -----------------------------

/// Auxiliary-vector keys musl reads at start-up.
const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_ENTRY: u64 = 9;
const AT_RANDOM: u64 = 25;

/// Writes the System V process stack into the stack's top page (`page`,
/// mapped at `USER_STACK_TOP - 4096`): argument strings and 16 random
/// bytes at the top, then (16-byte aligned) argc, argv, an empty
/// environment and the auxiliary vector. Returns the initial RSP.
fn linux_stack(
    mem: &Memory,
    page: PhysFrame,
    cmdline: &str,
    file: &[u8],
    image: &elf::Image,
) -> u64 {
    let base = USER_STACK_TOP - 4096;
    let kva = mem.phys_to_virt(page.start_address());
    let mut top = 4096usize;
    // SAFETY (all writes below): `kva` is the freshly mapped, zeroed top
    // stack page of the new process, reached through the physical-memory
    // window; every offset stays inside it (`cmdline` is at most
    // MAX_CMDLINE bytes and at most 64 arguments are kept).
    let put_bytes = |top: &mut usize, bytes: &[u8]| -> u64 {
        *top -= bytes.len();
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), kva.add(*top), bytes.len()) };
        base + *top as u64
    };
    // AT_RANDOM: stack-protector seed for musl; not cryptographic.
    // SAFETY: `rdtsc` only reads the time-stamp counter.
    let tsc = unsafe { core::arch::x86_64::_rdtsc() };
    let random: [u8; 16] =
        (u128::from(tsc).wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835)).to_le_bytes();
    let random_at = put_bytes(&mut top, &random);
    let mut argv = [0u64; 64];
    let mut argc = 0;
    for arg in cmdline.split('\0').take(argv.len()) {
        put_bytes(&mut top, &[0]);
        argv[argc] = put_bytes(&mut top, arg.as_bytes());
        argc += 1;
    }
    // Program headers: the ELF header's table, found in a loaded segment.
    let phoff = u64::from_le_bytes(file[32..40].try_into().unwrap_or([0; 8]));
    let phnum = u64::from(u16::from_le_bytes([file[56], file[57]]));
    let phdr = image
        .loadable()
        .find(|s| phoff >= s.file_offset && phoff < s.file_offset + s.file_size)
        .map_or(0, |s| s.vaddr + (phoff - s.file_offset));
    let aux = [
        (AT_PHDR, phdr),
        (AT_PHENT, 56),
        (AT_PHNUM, phnum),
        (AT_PAGESZ, 4096),
        (AT_ENTRY, image.entry),
        (AT_RANDOM, random_at),
        (AT_NULL, 0),
    ];
    // argc, argv[], NULL, envp NULL, auxv pairs.
    let words = 1 + argc + 1 + 1 + 2 * aux.len();
    let mut at = (top - words * 8) & !15;
    let rsp = base + at as u64;
    let mut word = |v: u64| {
        unsafe { (kva.add(at) as *mut u64).write_unaligned(v) };
        at += 8;
    };
    word(argc as u64);
    for &a in &argv[..argc] {
        word(a);
    }
    word(0);
    word(0);
    for (k, v) in aux {
        word(k);
        word(v);
    }
    rsp
}

/// Largest single write or writev accepted (bytes), so the kernel heap
/// copy stays small.
const LINUX_WRITE_MAX: u64 = 64 * 1024;

/// Linux programs' `brk` area (reserved on first use), and limits.
const LINUX_BRK_SIZE: u64 = 64 * 1024 * 1024;
const LINUX_PATH_MAX: usize = 256;
const LINUX_FILES_MAX: usize = 64;
const LINUX_RW: u64 = zoo_abi::prot::READ | zoo_abi::prot::WRITE;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;
const AT_EMPTY_PATH: u64 = 0x1000;

fn page_up(a: u64) -> u64 {
    a.div_ceil(4096) * 4096
}

/// A NUL-terminated user string into `buf`; its length, or `None` if it
/// is unreadable or too long.
fn user_cstr(ok: &impl Fn(u64, u64, bool) -> bool, addr: u64, buf: &mut [u8]) -> Option<usize> {
    for (k, slot) in buf.iter_mut().enumerate() {
        let a = addr.checked_add(k as u64)?;
        if !ok(a, 1, false) {
            return None;
        }
        // SAFETY: just checked as one readable byte of user memory.
        let b = unsafe { (a as *const u8).read() };
        if b == 0 {
            return Some(k);
        }
        *slot = b;
    }
    None
}

/// A Linux path as an initrd name: `/etc/services` → `etc/services`.
/// Relative paths start at `/`; `.` and `..` are refused.
fn initrd_path(path: &str) -> Option<&str> {
    let p = path.trim_start_matches('/');
    if p.is_empty() || p.split('/').any(|c| c == "." || c == "..") {
        return None;
    }
    Some(p)
}

/// Linux error numbers used here.
const ENOENT: i64 = 2;
const EBADF: i64 = 9;
const ENOMEM: i64 = 12;
const EMFILE: i64 = 24;
const EROFS: i64 = 30;
const EFAULT: i64 = 14;
const EINVAL: i64 = 22;
const ENOTTY: i64 = 25;
const ENOSYS: i64 = 38;

/// Linux system calls already reported as unsupported (numbers below 512).
static LINUX_REPORTED: [core::sync::atomic::AtomicU64; 8] =
    [const { core::sync::atomic::AtomicU64::new(0) }; 8];

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
        klog::push(pid, name, &text);
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
        // Preemption: plain round-robin, including the kernel task.
        s.prefer_user = false;
        let next = s.switch_away(frame, m);
        s.prefer_user = true;
        next
    })
    .unwrap_or(frame)
}

pub fn on_syscall(frame: *mut TrapFrame) -> *mut TrapFrame {
    syscall_with(frame, false)
}

/// A Linux system call from the `syscall` instruction.
pub fn on_linux_syscall(frame: *mut TrapFrame) -> *mut TrapFrame {
    syscall_with(frame, true)
}

fn syscall_with(frame: *mut TrapFrame, linux: bool) -> *mut TrapFrame {
    dispatch_locked(|s, m| {
        let cur = s.current;
        s.task(cur).frame = frame as u64;
        // SAFETY: the frame was just pushed for this trap on this task's
        // kernel stack.
        let f = unsafe { &mut *frame };
        let outcome = if linux {
            s.linux_syscall(cur, f, m)
        } else {
            s.syscall(cur, f, m)
        };
        match outcome {
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
            Outcome::ThreadExit(code) => {
                s.finish_thread(cur, code);
                s.switch_away(frame, m)
            }
            Outcome::Resume => frame,
            Outcome::Crash => {
                s.finish(cur, ExitStatus::Crashed);
                s.switch_away(frame, m)
            }
        }
    })
    .expect("system call before the scheduler started")
}

/// The kernel's crash marker line for a user fault.
fn crash_line(pid: u64, name: &str, vector: u64, error: u64, addr: u64, rip: u64) -> String {
    let mut line = String::new();
    let _ = write!(line, "ZOO_OS PROC crash pid={} name={} ", pid, name);
    if vector == trap::VEC_PAGE_FAULT {
        let (access, cause) = interrupts::page_fault_kind(error);
        let _ = write!(
            line,
            "fault=page-fault access={} cause={} addr={:#018x}",
            access, cause, addr
        );
    } else {
        let _ = write!(
            line,
            "fault={} rip={:#018x}",
            interrupts::fault_name(vector),
            rip
        );
    }
    line
}

pub fn on_user_fault(frame: *mut TrapFrame) -> *mut TrapFrame {
    dispatch_locked(|s, m| {
        let cur = s.current;
        // SAFETY: the frame was just pushed for this trap on this thread's
        // kernel stack.
        let f = unsafe { &mut *frame };
        let addr = if f.vector == trap::VEC_PAGE_FAULT {
            Cr2::read_raw()
        } else {
            0
        };
        if s.deliver_exception(cur, f, addr, m) {
            return frame;
        }
        let t = s.task(cur);
        let line = crash_line(t.pid, &t.name, f.vector, f.error, addr, f.rip);
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
