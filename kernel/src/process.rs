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
    /// Shared memory pages (docs/f1-design.md).
    Shm(Arc<ShmPages>),
}

/// Pages several processes can map. Each handle and each mapping holds a
/// reference; when the last goes, the frames are queued for the next reap
/// (which holds the memory lock) rather than freed here.
struct ShmPages {
    frames: Vec<PhysFrame>,
}

/// Frames of shared memory nobody refers to any more, freed by `reap`.
static SHM_FREED: Mutex<Vec<PhysFrame>> = Mutex::new(Vec::new());

impl Drop for ShmPages {
    fn drop(&mut self) {
        SHM_FREED.lock().append(&mut self.frames);
    }
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
    /// Shared memory mapped here: (address, pages, the pages), unmapped
    /// without freeing before the space is freed.
    shm_maps: Vec<(u64, u64, Arc<ShmPages>)>,
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
    linux_files: Vec<Option<LinuxFile>>,
    /// Linux programs' working directory, as an initrd path (`""` is `/`).
    linux_cwd: String,
    /// The home-folder handle (a channel to BadgerHandler), if given, and
    /// the shared buffer attached to that session for bulk transfers
    /// (docs/f1-design.md); only the kernel touches it on this side.
    linux_home: Option<u64>,
    linux_shm: Option<Arc<ShmPages>>,
    /// Pages committed through `vm_commit`.
    committed: u64,
    /// User exception handler entry (docs/posix3-design.md); 0 if none.
    exc_handler: u64,
    /// A Linux program (docs/linux1-design.md).
    linux: bool,
    /// Linux descriptors with close-on-exec set (bit `fd`).
    linux_cloexec: u64,
    /// The pid of the Linux program that started this one (0: the kernel),
    /// and the children this one started: pid and, once ended, the Linux
    /// wait status (docs/linux5-design.md).
    linux_parent: u64,
    linux_children: Vec<(u64, Option<i32>)>,
    /// Running in its parent's address space (`vfork`) until it execs or
    /// ends; the space is then not freed with this process.
    linux_borrowed: bool,
    /// The actions set with `rt_sigaction`: signal and `(handler, flags,
    /// restorer, mask)`, shared by the threads.
    linux_sigactions: Vec<(u8, [u64; 4])>,
    /// Signals sent to the process and not yet taken by a thread, and the
    /// pid that last sent each (docs/linux6-design.md).
    linux_pending: u64,
    linux_sigsrc: [u32; 64],
    /// The signal that ended the process, if one did.
    linux_term_sig: Option<u8>,
    /// Linux descriptors in non-blocking mode (bit `fd`, docs/linux7-design.md).
    linux_nonblock: u64,
    /// Anonymous memory committed when first touched (docs/linux6-design.md):
    /// (start, length, Linux protection) ranges inside reservations.
    linux_lazy: Vec<(u64, u64, u64)>,
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
    /// Linux file calls in progress (docs/linux4-design.md): the replies
    /// BadgerHandler has given this call so far, the next one the re-run
    /// asks for, and whether a request is waiting for its reply.
    fs_results: Vec<(i64, Vec<u8>)>,
    fs_cursor: usize,
    fs_pending: bool,
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
    /// A `vfork` waiting for this child to exec or end.
    vfork_child: Option<u64>,
    /// Linux signal state of this thread (docs/linux6-design.md): blocked
    /// signals, signals sent to it, the alternate stack (base, size,
    /// enabled), and the address cleared and woken when it exits.
    sigmask: u64,
    sig_pending: u64,
    altstack: (u64, u64, bool),
    clear_tid: u64,
    /// Between an `Open` reply and its folder handle (`fork`): the home
    /// channel is still in use.
    fs_hold: bool,
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
            fs_results: Vec::new(),
            fs_cursor: 0,
            fs_pending: false,
            fpu: None,
            fs_base: 0,
            futex: None,
            futex_woken: false,
            exc_stack: 0,
            in_exception: false,
            vfork_child: None,
            sigmask: 0,
            sig_pending: 0,
            altstack: (0, 0, false),
            clear_tid: 0,
            fs_hold: false,
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
            shm_maps: Vec::new(),
            log_ok: false,
            live: 1,
            members: 1,
            ended: false,
            vmas: Vec::new(),
            linux_brk: None,
            linux_files: Vec::new(),
            linux_cwd: String::new(),
            linux_home: None,
            linux_shm: None,
            committed: 0,
            exc_handler: 0,
            linux: false,
            linux_cloexec: 0,
            linux_parent: 0,
            linux_children: Vec::new(),
            linux_borrowed: false,
            linux_sigactions: Vec::new(),
            linux_pending: 0,
            linux_sigsrc: [0; 64],
            linux_term_sig: None,
            linux_lazy: Vec::new(),
            linux_nonblock: 0,
        }
    }

    fn set_nonblock(&mut self, fd: usize, on: bool) {
        if on {
            self.linux_nonblock |= 1 << fd;
        } else {
            self.linux_nonblock &= !(1 << fd);
        }
    }

    fn nonblock(&self, fd: u64) -> bool {
        fd < 64 && self.linux_nonblock >> fd & 1 != 0
    }

    fn set_cloexec(&mut self, fd: usize, on: bool) {
        if on {
            self.linux_cloexec |= 1 << fd;
        } else {
            self.linux_cloexec &= !(1 << fd);
        }
    }

    /// The `sa_flags` set for Linux signal `sig`.
    fn sig_flags(&self, sig: u64) -> u64 {
        self.linux_sigactions
            .iter()
            .find(|a| u64::from(a.0) == sig)
            .map_or(0, |a| a.1[1])
    }

    /// The handler set for Linux signal `sig` (0: default, 1: ignore).
    fn sig_handler(&self, sig: u64) -> u64 {
        self.linux_sigactions
            .iter()
            .find(|a| u64::from(a.0) == sig)
            .map_or(0, |a| a.1[0])
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
    /// Bound Unix socket names (docs/linux7-design.md): "@name" for the
    /// abstract name space, "/path" for paths. A name is free again once
    /// its socket is closed.
    unix_names: Vec<(String, alloc::sync::Weak<Mutex<Sock>>)>,
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
            unix_names: Vec::new(),
        });
    });
}

/// Halts until the next interrupt, allowing a switch to a user task. Call
/// with interrupts disabled; returns with interrupts disabled.
pub fn idle_hlt() {
    crate::serial::pump();
    crate::console::render();
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
        // The home folder the shell asked the supervisor for, if any.
        let home = s.pending_home.take();
        let (_, pid) = s.spawn_as(m, cmdline, [home, None], true)?;
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
        let image = program_image(file, linux).map_err(|e| {
            kprintln!("ZOO_OS INFO spawn {} rejected: {:?}", name, e);
            Error::Inval
        })?;
        // A dynamically linked Linux program names its loader
        // (docs/linux8-design.md).
        let interp = if linux {
            linux_interp(file, &image).map_err(|_| Error::NotFound)?
        } else {
            None
        };
        let (index, slot) = self.free_task_slots()?;
        let p4 = mem.new_address_space().map_err(|_| Error::NoMem)?;
        let Ok(stack_page) = load_image(mem, p4, file, &image) else {
            mem.free_address_space(p4);
            return Err(Error::NoMem);
        };
        if let Some((ifile, iimage)) = &interp
            && load_segments(mem, p4, ifile, iimage).is_err()
        {
            mem.free_address_space(p4);
            return Err(Error::NoMem);
        }
        let base = interp.as_ref().map_or(0, |x| x.1.bias);
        let entry = interp.as_ref().map_or(image.entry, |x| x.1.entry);
        let rsp = if linux {
            let args: Vec<&str> = cmdline.split('\0').take(LINUX_ARGS_MAX).collect();
            linux_stack(mem, stack_page, &args, &LINUX_ENV, file, &image, base)
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
        if linux {
            proc.linux = true;
            proc.linux_files = alloc::vec![
                Some(LinuxFile::Console { input: true }),
                Some(LinuxFile::Console { input: false }),
                Some(LinuxFile::Console { input: false }),
            ];
            proc.linux_home = (arg != NO_HANDLE).then_some(arg);
            proc.linux_shm = proc.linux_home.and_then(|h| attach_bulk(mem, &proc, h));
        }
        let pidx = self.add_proc(proc);
        let mut task = Task::new(pid, pid, name);
        // SysV: at function entry, rsp + 8 is 16-byte aligned.
        task.frame = Self::first_frame(top, entry, rsp, arg, arg2);
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
        let (pid, name, handles, files, parent, term) = {
            let p = self.procs[pidx].as_mut().expect("process slot in use");
            if p.ended {
                return;
            }
            p.ended = true;
            p.live = 0;
            let handles: Vec<HandleEntry> = p.handles.iter_mut().filter_map(Option::take).collect();
            let files = core::mem::take(&mut p.linux_files);
            (
                p.pid,
                p.name.clone(),
                handles,
                files,
                p.linux_parent,
                p.linux_term_sig,
            )
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
        // Pipe ends close here, so readers see the end of the data now.
        drop(files);
        if let Some(sig) = term {
            kprintln!("ZOO_OS PROC exit pid={} name={} signal={}", pid, name, sig);
        } else if let ExitStatus::Code(code) = status {
            kprintln!("ZOO_OS PROC exit pid={} name={} code={}", pid, name, code);
        }
        // The Linux wait status, for the parent's `wait4`.
        if parent != 0 {
            let wstatus = match (term, status) {
                (Some(sig), _) => i32::from(sig),
                (None, ExitStatus::Code(c)) => i32::from(c) << 8,
                (None, ExitStatus::Crashed) => SIGSEGV as i32,
                (None, ExitStatus::Killed) => SIGKILL as i32,
            };
            for p in self.procs.iter_mut().flatten() {
                if p.pid == parent
                    && let Some(c) = p.linux_children.iter_mut().find(|c| c.0 == pid)
                {
                    c.1 = Some(wstatus);
                }
            }
        }
        // `vfork` children running in this process's memory end with it.
        let borrowers: Vec<usize> = (0..self.tasks.len())
            .filter(|&k| {
                matches!(&self.tasks[k], Some(t) if t.state != State::Exited
                    && t.proc.is_some_and(|q| matches!(&self.procs[q],
                        Some(p) if p.linux_borrowed && p.linux_parent == pid)))
            })
            .collect();
        for k in borrowers {
            self.finish(k, ExitStatus::Killed);
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
        let linux = self.proc(i).linux;
        let t = self.task(i);
        t.state = State::Exited;
        t.restart = false;
        let (pid, tid) = (t.pid, t.tid);
        // Linux threads come and go often; their exits are not logged.
        if !linux {
            kprintln!(
                "ZOO_OS INFO thread exit pid={} tid={} code={}",
                pid,
                tid,
                code
            );
        }
        self.thread_exits.push((pid, tid, code));
        self.zombies.push(i);
        self.wake_all();
    }

    /// Frees exited tasks that are not the current one, and a process's
    /// memory once its last thread is freed.
    fn reap(&mut self, mem: &mut Memory) {
        // Shared memory released since the last reap.
        for f in core::mem::take(&mut *SHM_FREED.lock()) {
            let _ = mem.frames.deallocate(f);
        }
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
            for (va, pages, _) in &p.shm_maps {
                // Shared pages: freed when their last reference goes.
                mem.unmap_user_keep(p.p4, *va, *pages);
            }
            if !p.linux_borrowed {
                mem.free_address_space(p.p4);
            }
            self.thread_exits.retain(|&(pid, _, _)| pid != p.pid);
        }
        self.zombies = keep;
        // And what the processes just reaped held.
        for f in core::mem::take(&mut *SHM_FREED.lock()) {
            let _ = mem.frames.deallocate(f);
        }
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
                // SAFETY: the saved frame of the task resuming now, on its
                // own kernel stack.
                if self.linux_deliver(i, unsafe { &mut *frame }, mem) {
                    return frame;
                }
                i = self.pick_next();
                continue;
            }
            // SAFETY: a blocked task's saved frame stays valid on its own
            // kernel stack until it is resumed, which is now.
            let f = unsafe { &mut *frame };
            // A signal for a Linux thread ends its blocked call (EINTR or a
            // restart) and runs the handler instead.
            if self.linux_interrupt(i, f) {
                if let Some(area) = &self.task(i).fpu {
                    fpu::restore(area);
                }
                if self.linux_deliver(i, f, mem) {
                    return frame;
                }
                i = self.pick_next();
                continue;
            }
            self.task(i).restart = false;
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
                    if self.linux_deliver(i, f, mem) {
                        return frame;
                    }
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

    /// Linux descriptor `fd` of task `i`'s process.
    fn linux_file(&mut self, i: usize, fd: u64) -> Option<&mut LinuxFile> {
        self.proc(i)
            .linux_files
            .get_mut(usize::try_from(fd).ok()?)?
            .as_mut()
    }

    /// Adds a process to the table; returns its index.
    fn add_proc(&mut self, proc: Proc) -> usize {
        let pidx = match self.procs.iter().position(|p| p.is_none()) {
            Some(k) => k,
            None => {
                self.procs.push(None);
                self.procs.len() - 1
            }
        };
        self.procs[pidx] = Some(proc);
        pidx
    }

    /// A live thread of process `pid`.
    fn task_of_pid(&self, pid: u64) -> Option<usize> {
        self.tasks.iter().position(|t| {
            matches!(t, Some(t) if t.pid == pid && t.proc.is_some() && t.state != State::Exited)
        })
    }

    /// Puts `file` in the lowest free Linux descriptor at or above `min`.
    fn alloc_fd(
        &mut self,
        i: usize,
        min: usize,
        file: LinuxFile,
        cloexec: bool,
    ) -> Result<usize, i64> {
        if min >= LINUX_FILES_MAX {
            return Err(EINVAL);
        }
        let p = self.proc(i);
        let files = &mut p.linux_files;
        while files.len() < min {
            files.push(None);
        }
        let fd = match files.iter().skip(min).position(Option::is_none) {
            Some(k) => min + k,
            None if files.len() < LINUX_FILES_MAX => {
                files.push(None);
                files.len() - 1
            }
            None => return Err(EMFILE),
        };
        files[fd] = Some(file);
        p.set_cloexec(fd, cloexec);
        Ok(fd)
    }

    /// `write`/`writev` on descriptor `fd` (docs/linux5-design.md).
    fn linux_write(&mut self, i: usize, fd: u64, bytes: &[u8], mem: &Memory) -> Outcome {
        match self.linux_file(i, fd) {
            // Standard output and error go to the program's log.
            Some(LinuxFile::Console { input: false }) => {
                let t = self.task(i);
                let text = bytes.strip_suffix(b"\n").unwrap_or(bytes);
                log_user(t.pid, &t.name, text);
                Outcome::Ret(bytes.len() as i64)
            }
            Some(LinuxFile::Pipe(end)) if end.write => {
                let pipe = end.pipe.clone();
                self.pipe_write(i, &pipe, bytes, mem)
            }
            Some(LinuxFile::Home { .. }) => self.home_write(i, fd, bytes, mem),
            Some(LinuxFile::Sock(s)) => {
                let s = s.clone();
                self.sock_send(i, &s, bytes, Vec::new(), false, mem)
            }
            Some(LinuxFile::Event(e)) => {
                if bytes.len() < 8 {
                    return Outcome::Ret(-EINVAL);
                }
                let v = u64::from_le_bytes(bytes[..8].try_into().unwrap_or([0; 8]));
                if v == u64::MAX {
                    return Outcome::Ret(-EINVAL);
                }
                {
                    let mut ev = e.lock();
                    if ev.count.saturating_add(v) > EVENTFD_MAX {
                        return Outcome::Block;
                    }
                    ev.count += v;
                }
                self.wake_all();
                Outcome::Ret(8)
            }
            _ => Outcome::Ret(-EBADF),
        }
    }

    /// Sends `bytes` (and descriptors, attached to the first byte) on a
    /// connected Unix socket: its outgoing pipe.
    fn sock_send(
        &mut self,
        i: usize,
        sock: &Arc<Mutex<Sock>>,
        bytes: &[u8],
        fds: Vec<LinuxFile>,
        nosignal: bool,
        mem: &Memory,
    ) -> Outcome {
        let tx = match &*sock.lock() {
            Sock::Connected { tx: Some(tx), .. } => tx.pipe.clone(),
            Sock::Connected { tx: None, .. } => return Outcome::Ret(-EPIPE),
            _ => return Outcome::Ret(-ENOTCONN),
        };
        let (readers, at) = {
            let p = tx.lock();
            (p.readers, p.total_in)
        };
        if readers == 0 {
            return if nosignal {
                Outcome::Ret(-EPIPE)
            } else {
                self.linux_sigpipe(i)
            };
        }
        let r = self.pipe_write(i, &tx, bytes, mem);
        if matches!(r, Outcome::Ret(n) if n > 0) && !fds.is_empty() {
            tx.lock().fds.push_back((at, fds));
        }
        r
    }

    /// Receives from a connected Unix socket: like a pipe read, stopping
    /// before bytes that carry descriptors so each `recvmsg` gets the
    /// descriptors sent with its first byte (returned with the outcome).
    fn sock_recv(
        &mut self,
        i: usize,
        sock: &Arc<Mutex<Sock>>,
        pieces: &[(u64, u64)],
        mem: &mut Memory,
    ) -> (Outcome, Vec<LinuxFile>) {
        let rx = match &*sock.lock() {
            Sock::Connected { rx, .. } => rx.pipe.clone(),
            _ => return (Outcome::Ret(-ENOTCONN), Vec::new()),
        };
        let (start, limit) = {
            let p = rx.lock();
            let start = p.total_out;
            let next = p.fds.iter().map(|f| f.0).find(|&at| at > start);
            (start, next.map_or(usize::MAX, |at| (at - start) as usize))
        };
        let r = self.pipe_read(i, &rx, pieces, mem, limit);
        let mut got = Vec::new();
        if let Outcome::Ret(n) = r
            && n > 0
        {
            let mut stale = Vec::new();
            {
                let mut p = rx.lock();
                while let Some(&(at, _)) = p.fds.front() {
                    if at >= start + n as u64 {
                        break;
                    }
                    let (at, files) = p.fds.pop_front().expect("front checked");
                    if at == start {
                        got = files;
                    } else {
                        stale.push(files);
                    }
                }
            }
            // Dropped with the pipe's lock released (they may hold ends of
            // this same pipe).
            drop(stale);
        }
        (r, got)
    }

    /// Whether a Linux file is ready (POLL* bits) for poll, select and
    /// epoll.
    fn linux_ready(file: &LinuxFile) -> u64 {
        let pipe_side = |pipe: &Arc<Mutex<Pipe>>, write: bool| -> u64 {
            let p = pipe.lock();
            if write {
                if p.readers == 0 {
                    POLLERR
                } else if p.len < PIPE_SIZE {
                    POLLOUT
                } else {
                    0
                }
            } else {
                (if p.len > 0 { POLLIN } else { 0 }) | (if p.writers == 0 { POLLHUP } else { 0 })
            }
        };
        match file {
            // Standard input is at its end: readable (0 bytes).
            LinuxFile::Console { input: true } => POLLIN,
            LinuxFile::Console { input: false } => POLLOUT,
            LinuxFile::Pipe(end) => pipe_side(&end.pipe, end.write),
            LinuxFile::Sock(s) => match &*s.lock() {
                Sock::Fresh | Sock::Bound(_) => POLLOUT | POLLHUP,
                Sock::Listening { backlog, .. } => {
                    if backlog.is_empty() {
                        0
                    } else {
                        POLLIN
                    }
                }
                Sock::Connected { rx, tx, .. } => {
                    let r = pipe_side(&rx.pipe, false);
                    // The peer closed: readable to the end, and RDHUP.
                    let r = if r & POLLHUP != 0 {
                        r | POLLIN | POLLRDHUP
                    } else {
                        r
                    };
                    let w = tx.as_ref().map_or(0, |t| pipe_side(&t.pipe, true));
                    let hup = if r & POLLHUP != 0 && tx.is_none() {
                        POLLHUP
                    } else {
                        0
                    };
                    (r & !POLLHUP) | w | hup
                }
            },
            LinuxFile::Event(e) => {
                let ev = e.lock();
                (if ev.count > 0 { POLLIN } else { 0 })
                    | (if ev.count < EVENTFD_MAX { POLLOUT } else { 0 })
            }
            LinuxFile::Epoll(_) => 0,
            _ => POLLIN | POLLOUT,
        }
    }

    /// Ends a poll-style wait: the ready count now, or, with none ready,
    /// 0 once the timeout (nanoseconds; `None` for none) has passed, else
    /// blocks.
    fn linux_wait_result(&mut self, i: usize, count: i64, timeout: Option<u64>) -> Outcome {
        let t = self.task(i);
        if count > 0 || timeout == Some(0) {
            t.sleep_until = None;
            return Outcome::Ret(count);
        }
        if let Some(ns) = timeout {
            let tick_ns = 1_000_000_000 / u64::from(interrupts::TIMER_HZ);
            let now = interrupts::ticks();
            let deadline = *t
                .sleep_until
                .get_or_insert(now.saturating_add(ns.div_ceil(tick_ns).max(1)));
            if now >= deadline {
                t.sleep_until = None;
                return Outcome::Ret(0);
            }
        }
        Outcome::Block
    }

    /// True if a blocked call on a non-blocking descriptor (or with
    /// MSG_DONTWAIT) should fail with EAGAIN instead.
    fn linux_would_block_fails(&mut self, i: usize, f: &TrapFrame) -> bool {
        let fd = match f.rax {
            0 | 1 | 19 | 20 | 43 | 288 | 44 | 45 | 46 | 47 | 42 => f.rdi,
            _ => return false,
        };
        {
            let t = self.task(i);
            if t.fs_pending || t.fs_hold {
                return false;
            }
        }
        let dontwait = match f.rax {
            44 | 45 => f.r10 & MSG_DONTWAIT != 0,
            46 | 47 => f.rdx & MSG_DONTWAIT != 0,
            _ => false,
        };
        let nb = self.proc(i).nonblock(fd);
        (nb || dontwait)
            && matches!(
                self.linux_file(i, fd),
                Some(LinuxFile::Pipe(_) | LinuxFile::Sock(_) | LinuxFile::Event(_))
            )
    }

    /// Writes into a pipe: up to `PIPE_BUF` bytes all at once, more as
    /// room allows; blocks while it is full.
    fn pipe_write(
        &mut self,
        i: usize,
        pipe: &Arc<Mutex<Pipe>>,
        bytes: &[u8],
        mem: &Memory,
    ) -> Outcome {
        if bytes.is_empty() {
            return Outcome::Ret(0);
        }
        let n = {
            let mut p = pipe.lock();
            if p.readers == 0 {
                drop(p);
                return self.linux_sigpipe(i);
            }
            let room = PIPE_SIZE - p.len;
            if room == 0 || (bytes.len() <= PIPE_BUF && room < bytes.len()) {
                return Outcome::Block;
            }
            let n = bytes.len().min(room);
            let at = (p.head + p.len) % PIPE_SIZE;
            ring_copy(mem, &p.frames, at, bytes.as_ptr() as *mut u8, n, true);
            p.len += n;
            p.total_in += n as u64;
            n
        };
        self.wake_all();
        Outcome::Ret(n as i64)
    }

    /// Reads from a pipe into user `pieces`: what is there, blocking while
    /// it is empty and has writers; 0 once it is empty and has none.
    fn pipe_read(
        &mut self,
        i: usize,
        pipe: &Arc<Mutex<Pipe>>,
        pieces: &[(u64, u64)],
        mem: &mut Memory,
        limit: usize,
    ) -> Outcome {
        let p4 = self.task(i).p4.expect("Linux call from a user task");
        if pieces.iter().all(|x| x.1 == 0) {
            return Outcome::Ret(0);
        }
        // Commit lazily mapped buffers before taking the pipe's lock.
        for &(base, len) in pieces {
            if len > 0 {
                let n = len.min(PIPE_SIZE as u64);
                self.linux_commit(i, mem, base, n, true);
            }
        }
        let total = {
            let mut p = pipe.lock();
            if p.len == 0 {
                return if p.writers == 0 {
                    Outcome::Ret(0)
                } else {
                    Outcome::Block
                };
            }
            let mut total = 0;
            for &(base, len) in pieces {
                let n = (len as usize).min(p.len).min(limit - total);
                if n == 0 {
                    continue;
                }
                if !mem.user_range_ok(p4, base, n as u64, true) {
                    if total == 0 {
                        return Outcome::Ret(-EFAULT);
                    }
                    break;
                }
                ring_copy(mem, &p.frames, p.head, base as *mut u8, n, false);
                p.head = (p.head + n) % PIPE_SIZE;
                p.len -= n;
                p.total_out += n as u64;
                total += n;
                if p.len == 0 || total == limit {
                    break;
                }
            }
            total
        };
        self.wake_all();
        Outcome::Ret(total as i64)
    }

    /// A write to a pipe nobody reads: SIGPIPE, which ends the program by
    /// default; otherwise the write fails with EPIPE.
    fn linux_sigpipe(&mut self, i: usize) -> Outcome {
        let me = self.task(i).pid;
        self.linux_send(i, SIGPIPE, me, true);
        if self.proc(i).ended {
            Outcome::Exit(0)
        } else {
            Outcome::Ret(-EPIPE)
        }
    }

    /// Commits the lazily mapped pages of `[addr, addr + len)` in task
    /// `i`'s process with their recorded access (docs/linux6-design.md),
    /// then says whether the whole range is accessible (writable if
    /// `write`).
    fn linux_commit(
        &mut self,
        i: usize,
        mem: &mut Memory,
        addr: u64,
        len: u64,
        write: bool,
    ) -> bool {
        let Some(p4) = self.task(i).p4 else {
            return false;
        };
        if mem.user_range_ok(p4, addr, len, write) {
            return true;
        }
        let Some(end) = addr.checked_add(len.max(1)) else {
            return false;
        };
        if end > USER_HALF_END {
            return false;
        }
        let mut va = addr & !0xFFF;
        while va < end {
            if mem.user_translate(p4, va).is_none() {
                let Some(prot) = lazy_prot(&self.proc(i).linux_lazy, va) else {
                    return false;
                };
                if mem.frames.available() < 64 {
                    return false;
                }
                let (w, x) = (
                    prot & zoo_abi::prot::WRITE != 0,
                    prot & zoo_abi::prot::EXEC != 0,
                );
                if mem.map_user_page(p4, va, w, x).is_err() {
                    return false;
                }
                self.proc(i).committed += 1;
            }
            va += 4096;
        }
        mem.user_range_ok(p4, addr, len, write)
    }

    /// True if task `i` is a thread of a Linux program.
    fn is_linux(&self, i: usize) -> bool {
        matches!(&self.tasks[i], Some(t)
            if t.proc.is_some_and(|q| matches!(&self.procs[q], Some(p) if p.linux)))
    }

    /// Sends Linux signal `sig` (1..=64) from pid `from` to the process of
    /// task `k`, or (`thread`) to that thread (docs/linux6-design.md).
    /// Ignored signals are dropped. A default action that ends the program
    /// acts at once unless every thread that could take it blocks it.
    /// Otherwise the signal waits in a pending set until a thread that does
    /// not block it returns to user mode; blocked calls of such threads are
    /// woken so they can be interrupted.
    fn linux_send(&mut self, k: usize, sig: u64, from: u64, thread: bool) {
        let bit = 1u64 << (sig - 1);
        let handler = if sig == SIGKILL {
            0
        } else {
            self.proc(k).sig_handler(sig)
        };
        if handler == 1 || (handler == 0 && default_ignored(sig)) {
            return;
        }
        let pidx = self.task(k).proc;
        let blocked = sig != SIGKILL
            && if thread {
                self.task(k).sigmask & bit != 0
            } else {
                self.tasks
                    .iter()
                    .flatten()
                    .filter(|t| t.proc == pidx && t.state != State::Exited)
                    .all(|t| t.sigmask & bit != 0)
            };
        if handler == 0 && !blocked {
            self.proc(k).linux_term_sig = Some(sig as u8);
            self.finish(k, ExitStatus::Killed);
            return;
        }
        self.proc(k).linux_sigsrc[(sig - 1) as usize] = from as u32;
        if thread {
            self.task(k).sig_pending |= bit;
        } else {
            self.proc(k).linux_pending |= bit;
        }
        for (n, t) in self.tasks.iter_mut().enumerate() {
            if let Some(t) = t
                && t.proc == pidx
                && t.state == State::Blocked
                && t.sigmask & bit == 0
                && (!thread || n == k)
            {
                t.state = State::Ready;
            }
        }
    }

    /// The lowest pending signal task `i` does not block, dropping pending
    /// ones that are now ignored.
    fn next_deliverable(&mut self, i: usize) -> Option<u64> {
        loop {
            let mask = self.task(i).sigmask & !UNBLOCKABLE;
            let ready = (self.task(i).sig_pending | self.proc(i).linux_pending) & !mask;
            if ready == 0 {
                return None;
            }
            let sig = u64::from(ready.trailing_zeros()) + 1;
            let handler = if sig == SIGKILL {
                0
            } else {
                self.proc(i).sig_handler(sig)
            };
            if handler == 1 || (handler == 0 && default_ignored(sig)) {
                let bit = !(1u64 << (sig - 1));
                self.task(i).sig_pending &= bit;
                self.proc(i).linux_pending &= bit;
                continue;
            }
            return Some(sig);
        }
    }

    /// Runs one pending signal for Linux task `i`, about to return to user
    /// mode with frame `f`: its handler (a signal frame on the user stack)
    /// or its default action. Returns false if the process ended.
    fn linux_deliver(&mut self, i: usize, f: &mut TrapFrame, mem: &mut Memory) -> bool {
        if !self.is_linux(i) {
            return true;
        }
        let Some(sig) = self.next_deliverable(i) else {
            return true;
        };
        let bit = 1u64 << (sig - 1);
        let to_thread = self.task(i).sig_pending & bit != 0;
        if to_thread {
            self.task(i).sig_pending &= !bit;
        } else {
            self.proc(i).linux_pending &= !bit;
        }
        let handler = if sig == SIGKILL {
            0
        } else {
            self.proc(i).sig_handler(sig)
        };
        if handler > 1 {
            // siginfo: signo, errno, code (SI_TKILL or SI_USER), pid, uid.
            let mut info = [0u8; 128];
            info[0..4].copy_from_slice(&(sig as i32).to_le_bytes());
            let code: i32 = if to_thread { -6 } else { 0 };
            info[8..12].copy_from_slice(&code.to_le_bytes());
            let from = self.proc(i).linux_sigsrc[(sig - 1) as usize];
            info[16..20].copy_from_slice(&from.to_le_bytes());
            info[20..24].copy_from_slice(&LINUX_UID.to_le_bytes());
            if self.linux_handler_frame(i, f, sig, &info, (0, 0, 0), mem) {
                return true;
            }
            // No room for the frame: as Linux, a SIGSEGV that ends it.
            self.proc(i).linux_term_sig = Some(SIGSEGV as u8);
        } else {
            self.proc(i).linux_term_sig = Some(sig as u8);
        }
        self.finish(i, ExitStatus::Killed);
        false
    }

    /// Ends task `i`'s blocked Linux call because a signal will be handled
    /// (or end the program): the call returns EINTR, or, for a handler
    /// with SA_RESTART, runs again after it. Calls waiting for
    /// BadgerHandler or a vfork child are not interrupted.
    fn linux_interrupt(&mut self, i: usize, f: &mut TrapFrame) -> bool {
        if !self.is_linux(i) || f.vector != trap::VEC_LINUX {
            return false;
        }
        {
            let t = self.task(i);
            if t.fs_pending || t.fs_hold || t.vfork_child.is_some() {
                return false;
            }
        }
        let Some(sig) = self.next_deliverable(i) else {
            return false;
        };
        let handler = if sig == SIGKILL {
            0
        } else {
            self.proc(i).sig_handler(sig)
        };
        if handler > 1 {
            let restart = self.proc(i).sig_flags(sig) & SA_RESTART != 0;
            // pause and the sleeps never restart, as on Linux.
            if restart && !matches!(f.rax, 7 | 23 | 34 | 35 | 230 | 232 | 270 | 271 | 281 | 441) {
                // Back to the `syscall` instruction; RAX still holds the
                // call number.
                f.rip -= 2;
            } else {
                f.rax = (-EINTR) as u64;
            }
        }
        let t = self.task(i);
        t.restart = false;
        t.sleep_until = None;
        t.futex = None;
        t.futex_woken = false;
        t.fs_results.clear();
        t.fs_cursor = 0;
        true
    }

    /// Writes a Linux x86-64 signal frame (return address, `ucontext` with
    /// the registers, mask and alternate stack, `siginfo`, and the x87/SSE
    /// state) below the interrupted stack or on the alternate stack, and
    /// starts the handler there. `fault` is (trap number, error code, CR2).
    /// False if the frame cannot be written.
    fn linux_handler_frame(
        &mut self,
        i: usize,
        f: &mut TrapFrame,
        sig: u64,
        info: &[u8; 128],
        fault: (u64, u64, u64),
        mem: &mut Memory,
    ) -> bool {
        let Some([handler, flags, restorer, mask]) = self
            .proc(i)
            .linux_sigactions
            .iter()
            .find(|a| u64::from(a.0) == sig)
            .map(|a| a.1)
        else {
            return false;
        };
        if flags & SA_RESTORER == 0 {
            return false;
        }
        let Some(p4) = self.task(i).p4 else {
            return false;
        };
        let (ss_sp, ss_size, ss_on) = self.task(i).altstack;
        let on_alt = ss_on && f.rsp.wrapping_sub(ss_sp) < ss_size;
        let sp = if flags & SA_ONSTACK != 0 && ss_on && !on_alt {
            ss_sp.wrapping_add(ss_size)
        } else {
            f.rsp.wrapping_sub(128) // below the red zone
        };
        let fp = sp.wrapping_sub(512) & !63;
        // At the handler's entry, rsp + 8 is 16-byte aligned (SysV).
        let frame = (fp.wrapping_sub(SIGFRAME) & !15).wrapping_sub(8);
        if sp > USER_HALF_END
            || frame > sp
            || !self.linux_commit(i, mem, frame, fp + 512 - frame, true)
        {
            return false;
        }
        let _ = p4;
        let old_mask = self.task(i).sigmask;
        let mut fx = fpu::new_state();
        fpu::save(&mut fx);
        let mut buf = [0u8; SIGFRAME as usize];
        {
            let mut put = |at: usize, v: u64| buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
            put(0, restorer);
            // ucontext at 8: flags, link, stack (sp, flags, size) at 24.
            put(24, ss_sp);
            put(
                32,
                if on_alt {
                    SS_ONSTACK
                } else if ss_on {
                    0
                } else {
                    SS_DISABLE
                },
            );
            put(40, ss_size);
            // mcontext at 48: the registers in Linux's `sigcontext` order.
            let csgsfs = (f.cs & 0xFFFF) | ((f.ss & 0xFFFF) << 48);
            let g = [
                f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15, f.rdi, f.rsi, f.rbp, f.rbx,
                f.rdx, f.rax, f.rcx, f.rsp, f.rip, f.rflags, csgsfs, fault.1, fault.0, old_mask,
                fault.2, fp,
            ];
            for (k, v) in g.iter().enumerate() {
                put(48 + 8 * k, *v);
            }
            // The mask to restore, after the 256-byte mcontext.
            put(304, old_mask);
        }
        buf[312..440].copy_from_slice(info);
        // SAFETY: `[frame, fp + 512)` was checked as writable user memory of
        // the running thread's address space; `buf` and the FXSAVE image
        // fit inside it (`frame + SIGFRAME <= fp`).
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), frame as *mut u8, buf.len());
            core::ptr::copy_nonoverlapping(fpu::bytes(&fx).as_ptr(), fp as *mut u8, 512);
        }
        f.rdi = sig;
        f.rsi = frame + 312;
        f.rdx = frame + 8;
        f.rax = 0;
        f.rip = handler;
        f.rsp = frame;
        f.rflags = RFLAGS_IF | RFLAGS_RESERVED;
        let bit = 1u64 << (sig - 1);
        let defer = if flags & SA_NODEFER == 0 { bit } else { 0 };
        let t = self.task(i);
        t.sigmask = (t.sigmask | mask | defer) & !UNBLOCKABLE;
        if flags & SA_RESETHAND != 0 {
            self.proc(i)
                .linux_sigactions
                .retain(|a| u64::from(a.0) != sig);
        }
        true
    }

    /// A CPU fault in a Linux thread: runs the program's handler for the
    /// matching signal if it has one and does not block it (true), or
    /// records the signal that ends the program (false; the caller then
    /// crashes it).
    fn linux_fault(&mut self, i: usize, f: &mut TrapFrame, addr: u64, mem: &mut Memory) -> bool {
        if !self.is_linux(i) {
            return false;
        }
        // First touch of lazily mapped memory: commit the page and retry.
        if f.vector == 14
            && f.error & 1 == 0
            && addr < USER_HALF_END
            && self.linux_commit(i, mem, addr & !0xFFF, 1, f.error & 2 != 0)
        {
            return true;
        }
        // (signal, si_code, si_addr) per vector, as Linux reports them.
        let (sig, code, at) = match f.vector {
            14 => (SIGSEGV, if f.error & 1 == 0 { 1i32 } else { 2 }, addr),
            0 => (SIGFPE, 1, f.rip),
            16 | 19 => (SIGFPE, 0, f.rip),
            6 => (SIGILL, 2, f.rip),
            1 | 3 => (SIGTRAP, 0x80, f.rip),
            17 => (SIGBUS, 1, addr),
            _ => (SIGSEGV, 0x80, 0),
        };
        let handler = self.proc(i).sig_handler(sig);
        let blocked = self.task(i).sigmask & (1 << (sig - 1)) != 0;
        if handler > 1 && !blocked {
            let mut info = [0u8; 128];
            info[0..4].copy_from_slice(&(sig as i32).to_le_bytes());
            info[8..12].copy_from_slice(&code.to_le_bytes());
            info[16..24].copy_from_slice(&at.to_le_bytes());
            let cr2 = if f.vector == 14 { addr } else { 0 };
            if self.linux_handler_frame(i, f, sig, &info, (f.vector, f.error, cr2), mem) {
                return true;
            }
        }
        self.proc(i).linux_term_sig = Some(sig as u8);
        false
    }

    /// Wakes up to `n` Linux futex waiters on `addr` in task `i`'s process
    /// and (`requeue`) moves up to that many more to wait on another
    /// address. Returns how many were woken or moved.
    fn linux_futex_wake(
        &mut self,
        i: usize,
        addr: u64,
        n: usize,
        requeue: Option<(u64, usize)>,
    ) -> i64 {
        let pidx = self.task(i).proc;
        let (mut woken, mut moved) = (0usize, 0usize);
        for t in self.tasks.iter_mut().flatten() {
            if t.proc != pidx || t.futex != Some(addr) || t.futex_woken || t.state == State::Exited
            {
                continue;
            }
            if woken < n {
                t.futex_woken = true;
                t.state = State::Ready;
                woken += 1;
            } else if let Some((to, max)) = requeue
                && moved < max
            {
                t.futex = Some(to);
                moved += 1;
            } else {
                break;
            }
        }
        (woken + moved) as i64
    }

    /// `clone` for a thread of task `i`'s process (musl's `pthread_create`
    /// flags): same memory, descriptors and handlers; its own stack, TLS
    /// and signal mask; it resumes from this call with 0.
    #[allow(clippy::too_many_arguments)]
    fn linux_thread(
        &mut self,
        i: usize,
        f: &TrapFrame,
        mem: &mut Memory,
        flags: u64,
        stack: u64,
        ptid: u64,
        ctid: u64,
        tls: u64,
    ) -> Outcome {
        let p4 = self.task(i).p4.expect("Linux call from a user task");
        if stack == 0
            || stack >= USER_HALF_END
            || (flags & CLONE_SETTLS != 0 && tls >= USER_HALF_END)
        {
            return Outcome::Ret(-EINVAL);
        }
        let ids = [(CLONE_PARENT_SETTID, ptid), (CLONE_CHILD_SETTID, ctid)];
        for (flag, at) in ids {
            if flags & flag != 0 && (at % 4 != 0 || !self.linux_commit(i, mem, at, 4, true)) {
                return Outcome::Ret(-EFAULT);
            }
        }
        let Ok((index, slot)) = self.free_task_slots() else {
            return Outcome::Ret(-EAGAIN);
        };
        let Ok(top) = mem.map_kernel_stack(slot) else {
            return Outcome::Ret(-ENOMEM);
        };
        self.kstack_used[slot] = true;
        let tid = self.next_tid;
        self.next_tid += 1;
        let mut frame = *f;
        frame.rax = 0;
        frame.rsp = stack;
        let frame_addr = top - core::mem::size_of::<TrapFrame>() as u64;
        // SAFETY: the kernel stack `[top - KSTACK_SIZE, top)` was just mapped
        // writable and belongs to the new thread alone; the frame fits.
        unsafe { (frame_addr as *mut TrapFrame).write(frame) };
        let mut area = Box::new(fpu::new_state());
        fpu::save(&mut area);
        let (pid, pidx, name, fs_base, sigmask) = {
            let t = self.task(i);
            (t.pid, t.proc, t.name.clone(), t.fs_base, t.sigmask)
        };
        let mut task = Task::new(pid, tid, &name);
        task.frame = frame_addr;
        task.fpu = Some(area);
        task.p4 = Some(p4);
        task.proc = pidx;
        task.kstack = Some((slot, top));
        task.fs_base = if flags & CLONE_SETTLS != 0 {
            tls
        } else {
            fs_base
        };
        task.sigmask = sigmask;
        task.clear_tid = if flags & CLONE_CHILD_CLEARTID != 0 {
            ctid
        } else {
            0
        };
        self.tasks[index] = Some(task);
        let p = self.proc(i);
        p.live += 1;
        p.members += 1;
        for (flag, at) in ids {
            if flags & flag != 0 {
                // SAFETY: checked above as writable, aligned user memory
                // of this (shared) address space.
                unsafe { (at as *mut u32).write_volatile(tid as u32) };
            }
        }
        Outcome::Ret(tid as i64)
    }

    /// The folder handle BadgerHandler sends after an `Open` reply on task
    /// `i`'s home channel: `None` while it has not arrived, `Some(None)` if
    /// something else came or the channel closed.
    fn home_handle(&mut self, i: usize) -> Option<Option<HandleEntry>> {
        let Some(h) = self.proc(i).linux_home else {
            return Some(None);
        };
        let Some(HandleEntry {
            object: Object::Endpoint { chan, side },
            ..
        }) = self.proc(i).get(h).cloned()
        else {
            return Some(None);
        };
        let mut c = chan.lock();
        match c.queues[side].front() {
            None if c.open[1 - side] => None,
            Some(Msg::Handle(..)) => match c.queues[side].pop_front() {
                Some(Msg::Handle(e, _)) => Some(Some(e)),
                _ => Some(None),
            },
            _ => Some(None),
        }
    }

    /// `fork`, `vfork` and `clone(CLONE_VM | CLONE_VFORK)` for task `i`
    /// (docs/linux5-design.md). The child gets a copy of the address space,
    /// or (`share`) runs in the parent's until it execs or ends while the
    /// parent waits; it gets the descriptors and its own home-folder
    /// session, and resumes from this call with 0 (on `stack`, if given).
    fn linux_fork(
        &mut self,
        i: usize,
        f: &TrapFrame,
        mem: &mut Memory,
        share: bool,
        stack: u64,
    ) -> Outcome {
        if let Some(child) = self.task(i).vfork_child {
            let waiting = self
                .procs
                .iter()
                .flatten()
                .any(|p| p.pid == child && p.linux_borrowed && !p.ended);
            if waiting {
                return Outcome::Block;
            }
            self.task(i).vfork_child = None;
            return Outcome::Ret(child as i64);
        }
        {
            let p = self.proc(i);
            if !p.file_maps.is_empty() || !p.shm_maps.is_empty() || p.display_pages > 0 {
                return Outcome::Ret(-ENOSYS);
            }
        }
        // The child's own home-folder session: a separate channel, so the
        // two programs' requests and replies never mix.
        let home = if self.proc(i).linux_home.is_some() {
            let (s, _) = step!(self.fs(i, Op::Open, zoo_abi::dir_rights::ALL, 0, "", &[]));
            if s < 0 {
                return Outcome::Ret(-zoo_errno(s));
            }
            let got = self.home_handle(i);
            self.task(i).fs_hold = got.is_none();
            match got {
                None => return Outcome::Block,
                Some(None) => return Outcome::Ret(-EIO),
                Some(Some(e)) => Some(e),
            }
        } else {
            None
        };
        let give_back = |e: Option<HandleEntry>| {
            if let Some(e) = e {
                release(e);
            }
        };
        let Ok((index, slot)) = self.free_task_slots() else {
            give_back(home);
            return Outcome::Ret(-EAGAIN);
        };
        let parent_p4 = self.proc(i).p4;
        let p4 = if share {
            parent_p4
        } else {
            match mem.copy_address_space(parent_p4) {
                Ok(p) => p,
                Err(_) => {
                    give_back(home);
                    return Outcome::Ret(-ENOMEM);
                }
            }
        };
        let Ok(top) = mem.map_kernel_stack(slot) else {
            if !share {
                mem.free_address_space(p4);
            }
            give_back(home);
            return Outcome::Ret(-ENOMEM);
        };
        self.kstack_used[slot] = true;
        let pid = self.next_pid;
        self.next_pid += 1;
        let parent = self.proc(i);
        parent.linux_children.push((pid, None));
        let mut child = Proc::new(pid, &parent.name, p4);
        child.cmdline = parent.cmdline.clone();
        child.heap_next = parent.heap_next;
        child.map_next = parent.map_next;
        child.vmas = parent.vmas.clone();
        child.committed = parent.committed;
        child.linux = true;
        child.linux_brk = parent.linux_brk;
        child.linux_files = parent.linux_files.clone();
        child.linux_cloexec = parent.linux_cloexec;
        child.linux_nonblock = parent.linux_nonblock;
        child.linux_cwd = parent.linux_cwd.clone();
        child.linux_parent = parent.pid;
        child.linux_borrowed = share;
        child.linux_sigactions = parent.linux_sigactions.clone();
        child.linux_lazy = parent.linux_lazy.clone();
        if let Some(e) = home {
            let h = child.insert(e).expect("new process has free handle slots");
            child.linux_home = Some(h);
            child.linux_shm = attach_bulk(mem, &child, h);
        }
        let name = child.name.clone();
        let pidx = self.add_proc(child);
        let mut frame = *f;
        frame.rax = 0;
        if stack != 0 {
            frame.rsp = stack;
        }
        let frame_addr = top - core::mem::size_of::<TrapFrame>() as u64;
        // SAFETY: the kernel stack `[top - KSTACK_SIZE, top)` was just mapped
        // writable and belongs to the new task alone; the frame fits at its
        // top.
        unsafe { (frame_addr as *mut TrapFrame).write(frame) };
        // The kernel does not use x87/SSE, so the registers still hold the
        // parent's state.
        let mut area = Box::new(fpu::new_state());
        fpu::save(&mut area);
        let (fs_base, sigmask, altstack) = {
            let t = self.task(i);
            (t.fs_base, t.sigmask, t.altstack)
        };
        let mut task = Task::new(pid, pid, &name);
        task.sigmask = sigmask;
        task.altstack = altstack;
        task.frame = frame_addr;
        task.fpu = Some(area);
        task.p4 = Some(p4);
        task.proc = Some(pidx);
        task.kstack = Some((slot, top));
        task.fs_base = fs_base;
        self.tasks[index] = Some(task);
        kprintln!("ZOO_OS PROC start pid={} name={}", pid, name);
        if share {
            self.task(i).vfork_child = Some(pid);
            return Outcome::Block;
        }
        Outcome::Ret(pid as i64)
    }

    /// `execve`: replaces task `i`'s program with `linux/<name>` from the
    /// initrd (docs/linux5-design.md). Nothing changes unless it succeeds.
    fn linux_exec(
        &mut self,
        i: usize,
        f: &mut TrapFrame,
        mem: &mut Memory,
        path: &str,
        argv: Vec<String>,
        env: Vec<String>,
    ) -> Outcome {
        let fail = |e: i64| Outcome::Ret(-e);
        let name = match path.strip_prefix("linux/") {
            Some(n) if valid_name(n) => n,
            _ if initrd_kind(path).is_some() || self.home_rel(i, path).is_some() => {
                return fail(EACCES);
            }
            _ => return fail(ENOENT),
        };
        let Some(file) = initrd::find(path) else {
            return fail(ENOENT);
        };
        let Ok(image) = program_image(file, true) else {
            return fail(ENOEXEC);
        };
        let interp = match linux_interp(file, &image) {
            Ok(x) => x,
            Err(e) => return fail(e),
        };
        // Strings, pointers and auxiliary vector must fit the stack's top page.
        let bytes: usize = argv.iter().chain(&env).map(|s| s.len() + 1).sum();
        let words = 1 + argv.len() + 1 + env.len() + 1 + 2 * LINUX_AUX_COUNT;
        if argv.len() > LINUX_ARGS_MAX
            || env.len() > LINUX_ARGS_MAX
            || bytes + 16 + words * 8 + 16 > 4096
        {
            return fail(E2BIG);
        }
        let Ok(p4) = mem.new_address_space() else {
            return fail(ENOMEM);
        };
        let Ok(stack_page) = load_image(mem, p4, file, &image) else {
            mem.free_address_space(p4);
            return fail(ENOMEM);
        };
        if let Some((ifile, iimage)) = &interp
            && load_segments(mem, p4, ifile, iimage).is_err()
        {
            mem.free_address_space(p4);
            return fail(ENOMEM);
        }
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        let vars: Vec<&str> = env.iter().map(String::as_str).collect();
        let base = interp.as_ref().map_or(0, |x| x.1.bias);
        let entry = interp.as_ref().map_or(image.entry, |x| x.1.entry);
        let rsp = linux_stack(mem, stack_page, &args, &vars, file, &image, base);
        // From here on the call succeeds.
        let pid = self.task(i).pid;
        let p = self.proc(i);
        let (old, borrowed) = (p.p4, p.linux_borrowed);
        p.p4 = p4;
        p.linux_borrowed = false;
        p.name = String::from(name);
        p.cmdline = args.join("\0");
        p.vmas.clear();
        p.linux_lazy.clear();
        p.linux_brk = None;
        p.committed = 0;
        p.heap_next = HEAP_BASE;
        p.map_next = MAP_BASE;
        p.exc_handler = 0;
        for fd in 0..p.linux_files.len() {
            if p.linux_cloexec >> fd & 1 != 0 {
                p.linux_files[fd] = None;
                p.linux_nonblock &= !(1 << fd);
            }
        }
        p.linux_cloexec = 0;
        // Caught signals return to their default action; ignored stay so.
        p.linux_sigactions.retain(|a| a.1[0] == 1);
        let t = self.task(i);
        t.p4 = Some(p4);
        t.name = String::from(name);
        t.fs_base = 0;
        t.altstack = (0, 0, false);
        t.clear_tid = 0;
        let area = Box::new(fpu::new_state());
        fpu::restore(&area);
        t.fpu = Some(area);
        FsBase::write(VirtAddr::new(0));
        // SAFETY: `p4` maps the kernel half like every process table, so the
        // running kernel code and stack stay mapped; this task no longer
        // uses the old table.
        unsafe { Cr3::write(p4, Cr3::read().1) };
        if !borrowed {
            mem.free_address_space(old);
        }
        let sel = gdt::selectors();
        *f = TrapFrame {
            rip: entry,
            cs: sel.user_code.0 as u64,
            rflags: RFLAGS_IF | RFLAGS_RESERVED,
            rsp,
            ss: sel.user_data.0 as u64,
            vector: f.vector,
            ..TrapFrame::default()
        };
        kprintln!("ZOO_OS PROC exec pid={} name={}", pid, name);
        self.wake_all();
        Outcome::Resume
    }

    /// The user path at `addr`, resolved against the working directory.
    fn linux_path(
        &mut self,
        i: usize,
        ok: &impl Fn(u64, u64, bool) -> bool,
        addr: u64,
    ) -> Option<String> {
        let mut name = [0u8; LINUX_PATH_MAX];
        let n = user_cstr(ok, addr, &mut name)?;
        let path = core::str::from_utf8(&name[..n]).ok()?;
        Some(linux_resolve(&self.proc(i).linux_cwd, path))
    }

    /// A Linux x86-64 system call (docs/linux1-design.md): number in RAX,
    /// arguments in RDI, RSI, RDX, R10, R8, R9; errors as negative errno.
    fn linux_syscall(&mut self, i: usize, f: &mut TrapFrame, mem: &mut Memory) -> Outcome {
        let mut outcome;
        let mut tries = 0;
        loop {
            // A re-run replays the replies this call already has (`fs`).
            self.task(i).fs_cursor = 0;
            // The first user range the call found inaccessible: if it is
            // lazily mapped memory, commit it and run the call again
            // (docs/linux6-design.md).
            let want = core::cell::Cell::new(None);
            outcome = self.linux_call(i, f, mem, &want);
            match (&outcome, want.get()) {
                (Outcome::Ret(v), Some((addr, len, write)))
                    if *v < 0 && tries < 8 && self.linux_commit(i, mem, addr, len, write) =>
                {
                    tries += 1;
                }
                _ => break,
            }
        }
        if matches!(outcome, Outcome::Block) && self.linux_would_block_fails(i, f) {
            outcome = Outcome::Ret(-EAGAIN);
        }
        if !matches!(outcome, Outcome::Block) {
            let t = self.task(i);
            t.fs_results.clear();
            t.fs_pending = false;
        }
        outcome
    }

    /// One request to BadgerHandler for task `i`'s Linux call
    /// (docs/linux4-design.md). The `k`-th request of a call is sent once;
    /// the call blocks until its reply arrives and is then re-run, getting
    /// the earlier replies back in order without asking again.
    fn fs(&mut self, i: usize, op: Op, arg: u64, count: u16, path: &str, data: &[u8]) -> FsStep {
        let k = self.task(i).fs_cursor;
        self.task(i).fs_cursor += 1;
        if let Some((s, b)) = self.task(i).fs_results.get(k) {
            return FsStep::Done(*s, b.clone());
        }
        // Threads of a process share its channel: one request at a time,
        // so each reply goes to the thread that asked.
        if !self.task(i).fs_pending {
            let pidx = self.task(i).proc;
            let busy = self.tasks.iter().enumerate().any(|(k, t)| {
                k != i && matches!(t, Some(t) if t.proc == pidx && (t.fs_pending || t.fs_hold))
            });
            if busy {
                return FsStep::Wait;
            }
        }
        let Some(h) = self.proc(i).linux_home else {
            return FsStep::Fail(ENOENT);
        };
        let (chan, side) = match self.proc(i).get(h) {
            Some(HandleEntry {
                object: Object::Endpoint { chan, side },
                ..
            }) => (chan.clone(), *side),
            _ => return FsStep::Fail(EIO),
        };
        if !self.task(i).fs_pending {
            // Request: op | arg u64 | count u16 | path_len u8 | path | data.
            if path.len() > 255 || 12 + path.len() + data.len() > MAX_MESSAGE {
                return FsStep::Fail(ENAMETOOLONG);
            }
            let mut m = alloc::vec::Vec::with_capacity(12 + path.len() + data.len());
            m.push(op as u8);
            m.extend_from_slice(&arg.to_le_bytes());
            m.extend_from_slice(&count.to_le_bytes());
            m.push(path.len() as u8);
            m.extend_from_slice(path.as_bytes());
            m.extend_from_slice(data);
            {
                let mut c = chan.lock();
                if !c.open[1 - side] {
                    return FsStep::Fail(EIO);
                }
                if c.queues[1 - side].len() >= CHANNEL_DEPTH {
                    return FsStep::Fail(EAGAIN);
                }
                c.queues[1 - side].push_back(Msg::Bytes(m));
            }
            self.task(i).fs_pending = true;
            self.wake_all();
            return FsStep::Wait;
        }
        // Reply: status i64, then the result bytes.
        let reply = {
            let mut c = chan.lock();
            match c.queues[side].front() {
                None if c.open[1 - side] => return FsStep::Wait,
                None => None,
                Some(Msg::Bytes(_)) => match c.queues[side].pop_front() {
                    Some(Msg::Bytes(b)) => Some(b),
                    _ => None,
                },
                Some(Msg::Handle(..)) => None,
            }
        };
        self.task(i).fs_pending = false;
        // Another thread may be waiting for the channel.
        self.wake_all();
        let Some(b) = reply.filter(|b| b.len() >= 8) else {
            return FsStep::Fail(EIO);
        };
        let status = i64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8]));
        let payload = b[8..].to_vec();
        self.task(i).fs_results.push((status, payload.clone()));
        FsStep::Done(status, payload)
    }

    /// The home-folder path (relative to it) for a resolved path, when this
    /// process has the home folder.
    fn home_rel(&mut self, i: usize, path: &str) -> Option<String> {
        self.proc(i).linux_home?;
        if path == "home/user" {
            return Some(String::new());
        }
        path.strip_prefix("home/user/").map(String::from)
    }

    /// write/writev to descriptor `fd` (3 and up): one request with as
    /// much as fits in a message; a short write, which callers repeat.
    fn home_write(&mut self, i: usize, fd: u64, bytes: &[u8], mem: &Memory) -> Outcome {
        let fail = |e: i64| Outcome::Ret(-e);
        let (path, pos, append) = match self.linux_file(i, fd).cloned() {
            Some(LinuxFile::Home {
                path,
                pos,
                write: true,
                append,
            }) => (path, pos, append),
            Some(_) | None => return fail(EBADF),
        };
        if bytes.is_empty() {
            return Outcome::Ret(0);
        }
        let at = if append {
            let (s, entry) = step!(self.fs(i, Op::Stat, 0, 0, &path, &[]));
            if s < 0 {
                return fail(zoo_errno(s));
            }
            entry_size(&entry)
        } else {
            pos
        };
        let n = if let Some(shm) = self.proc(i).linux_shm.clone() {
            // Up to the shared buffer per request (docs/f1-design.md).
            let chunk = &bytes[..bytes.len().min(LINUX_BULK)];
            bytes_to_shm(mem, &shm, chunk);
            let len32 = (chunk.len() as u32).to_le_bytes();
            let (s, _) = step!(self.fs(i, Op::WriteBulk, at, 0, &path, &len32));
            if s < 0 {
                return fail(zoo_errno(s));
            }
            (s as usize).min(chunk.len())
        } else {
            let room = MAX_MESSAGE.saturating_sub(12 + path.len());
            let chunk = &bytes[..bytes.len().min(room)];
            let (s, _) = step!(self.fs(i, Op::Write, at, 0, &path, chunk));
            if s < 0 {
                return fail(zoo_errno(s));
            }
            (s as usize).min(chunk.len())
        };
        if let Some(LinuxFile::Home { pos, .. }) = self.linux_file(i, fd) {
            *pos = at + n as u64;
        }
        Outcome::Ret(n as i64)
    }

    fn linux_call(
        &mut self,
        i: usize,
        f: &mut TrapFrame,
        mem: &mut Memory,
        want: &core::cell::Cell<Option<(u64, u64, bool)>>,
    ) -> Outcome {
        let (number, a1, a2, a3) = (f.rax, f.rdi, f.rsi, f.rdx);
        let p4 = self.task(i).p4.expect("system call from a user task");
        let ok = |addr: u64, len: u64, write: bool| {
            let good = mem.user_range_ok(p4, addr, len, write);
            if !good && want.get().is_none() {
                want.set(Some((addr, len, write)));
            }
            good
        };
        let fail = |e: i64| Outcome::Ret(-e);
        match number {
            // write(fd, buf, len)
            1 => {
                if a3 > 0 && !ok(a2, a3, false) {
                    return fail(EFAULT);
                }
                // SAFETY: just checked as readable user memory of the
                // running process.
                let bytes = unsafe { core::slice::from_raw_parts(a2 as *const u8, a3 as usize) };
                self.linux_write(i, a1, bytes, mem)
            }
            // writev(fd, iov, count)
            20 => {
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
                self.linux_write(i, a1, &all, mem)
            }
            // ioctl: no terminals yet.
            // ioctl: FIONBIO and FIONREAD; no terminals yet.
            16 => {
                if self.linux_file(i, a1).is_none() {
                    return fail(EBADF);
                }
                match a2 {
                    0x5421 => {
                        if !ok(a3, 4, false) {
                            return fail(EFAULT);
                        }
                        // SAFETY: checked as readable user memory.
                        let on = unsafe { (a3 as *const i32).read_unaligned() } != 0;
                        self.proc(i).set_nonblock(a1 as usize, on);
                        Outcome::Ret(0)
                    }
                    0x541B => {
                        let n = match self.linux_file(i, a1) {
                            Some(LinuxFile::Pipe(end)) => end.pipe.lock().len,
                            Some(LinuxFile::Sock(s)) => match &*s.lock() {
                                Sock::Connected { rx, .. } => rx.pipe.lock().len,
                                _ => 0,
                            },
                            _ => return fail(ENOTTY),
                        };
                        if !ok(a3, 4, true) {
                            return fail(EFAULT);
                        }
                        // SAFETY: checked as writable user memory.
                        unsafe { (a3 as *mut i32).write_unaligned(n as i32) };
                        Outcome::Ret(0)
                    }
                    _ => fail(ENOTTY),
                }
            }
            // getpid, gettid
            39 => Outcome::Ret(self.task(i).pid as i64),
            186 => Outcome::Ret(self.task(i).tid as i64),
            // exit, exit_group
            // exit: this thread (the last one ends the process), clearing
            // and waking its clear-tid word; exit_group: the whole process.
            60 => {
                let ctid = self.task(i).clear_tid;
                if ctid != 0 && ctid.is_multiple_of(4) && ok(ctid, 4, true) {
                    // SAFETY: checked as writable, aligned user memory.
                    unsafe { (ctid as *mut u32).write_volatile(0) };
                    self.linux_futex_wake(i, ctid, 1, None);
                }
                Outcome::ThreadExit(a1 as u8)
            }
            231 => Outcome::Exit(a1 as u8),
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
            // mmap(addr, len, prot, flags, fd, offset) (docs/linux8-design.md):
            // anonymous memory (committed when first touched) or a private
            // copy of an initrd file's pages; at an address the kernel
            // chooses, or (MAP_FIXED) at `addr`, replacing what a
            // reservation had there.
            9 => {
                let (addr, prot, flags, fd, off) = (a1, a3, f.r10, f.r8, f.r9);
                if a2 == 0 || !off.is_multiple_of(4096) {
                    return fail(EINVAL);
                }
                let len = page_up(a2);
                let data: Option<&'static [u8]> = if flags & MAP_ANONYMOUS != 0 {
                    None
                } else {
                    match self.linux_file(i, fd) {
                        Some(LinuxFile::File(d, _)) => Some(*d),
                        Some(_) => return fail(ENODEV),
                        None => return fail(EBADF),
                    }
                };
                let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
                let base = if fixed {
                    if !addr.is_multiple_of(4096) {
                        return fail(EINVAL);
                    }
                    let Some(end) = addr.checked_add(len) else {
                        return fail(ENOMEM);
                    };
                    let vmas = &self.proc(i).vmas;
                    let inside = vmas.iter().any(|&(s, l)| addr >= s && end <= s + l);
                    let overlaps = vmas.iter().any(|&(s, l)| addr < s + l && s < end);
                    if overlaps && flags & MAP_FIXED_NOREPLACE != 0 {
                        return fail(EEXIST);
                    }
                    if inside {
                        let _ = self.vm_call(i, sys::VM_DECOMMIT, addr, len, 0, p4, mem);
                        lazy_cut(&mut self.proc(i).linux_lazy, addr, len);
                    } else if overlaps
                        || !matches!(
                            self.vm_call(i, sys::VM_RESERVE, addr, len, 0, p4, mem),
                            Outcome::Ret(b) if b as u64 == addr
                        )
                    {
                        return fail(ENOMEM);
                    }
                    addr
                } else {
                    match self.vm_call(i, sys::VM_RESERVE, 0, len, 0, p4, mem) {
                        Outcome::Ret(b) if b > 0 => b as u64,
                        _ => return fail(ENOMEM),
                    }
                };
                if prot & 7 == 0 {
                    return Outcome::Ret(base as i64);
                }
                let prot = (prot & 7) | zoo_abi::prot::READ;
                match data {
                    // Committed page by page when first touched.
                    None => self.proc(i).linux_lazy.push((base, len, prot)),
                    Some(d) => {
                        let (w, x) = (prot & 2 != 0, prot & 4 != 0);
                        for k in 0..len / 4096 {
                            let va = base + k * 4096;
                            let Ok(frame) = mem.map_user_page(p4, va, w, x) else {
                                let _ = self.vm_call(i, sys::VM_DECOMMIT, base, len, 0, p4, mem);
                                return fail(ENOMEM);
                            };
                            self.proc(i).committed += 1;
                            let at = off + k * 4096;
                            if at < d.len() as u64 {
                                let n = (d.len() as u64 - at).min(4096) as usize;
                                let to = mem.phys_to_virt(frame.start_address());
                                // SAFETY: `frame` is the fresh page just mapped
                                // here, reached through the physical window;
                                // `at..at + n` lies in the file and `n` fits.
                                unsafe {
                                    core::ptr::copy_nonoverlapping(d[at as usize..].as_ptr(), to, n)
                                };
                            }
                        }
                    }
                }
                Outcome::Ret(base as i64)
            }
            // pread64(fd, buf, count, offset): initrd files.
            17 => {
                let Some(LinuxFile::File(data, _)) = self.linux_file(i, a1).cloned() else {
                    return fail(if self.linux_file(i, a1).is_some() {
                        ESPIPE
                    } else {
                        EBADF
                    });
                };
                let at = (f.r10 as usize).min(data.len());
                let n = (a3 as usize).min(data.len() - at);
                if n > 0 {
                    if !ok(a2, n as u64, true) {
                        return fail(EFAULT);
                    }
                    // SAFETY: checked as writable user memory; inside the file.
                    unsafe {
                        core::ptr::copy_nonoverlapping(data[at..].as_ptr(), a2 as *mut u8, n)
                    };
                }
                Outcome::Ret(n as i64)
            }
            // munmap(addr, len): a whole area is released, part of one is
            // decommitted; nothing mapped there is not an error.
            11 => {
                if a1 % 4096 != 0 || a2 == 0 {
                    return fail(EINVAL);
                }
                let len = page_up(a2);
                lazy_cut(&mut self.proc(i).linux_lazy, a1, len);
                let whole = self.proc(i).vmas.iter().any(|&(s, l)| s == a1 && l == len);
                if whole {
                    let _ = self.vm_call(i, sys::VM_RELEASE, a1, 0, 0, p4, mem);
                } else {
                    let _ = self.vm_call(i, sys::VM_DECOMMIT, a1, len, 0, p4, mem);
                }
                Outcome::Ret(0)
            }
            // mprotect(addr, len, prot): committed pages change now; inside a
            // reservation, the others get the new access when first touched
            // (`mmap` with PROT_NONE and then mprotect, as musl does for
            // thread stacks, costs nothing until used). PROT_NONE makes
            // untouched pages inaccessible; committed pages stay readable.
            10 => {
                if !a1.is_multiple_of(4096) {
                    return fail(EINVAL);
                }
                let len = page_up(a2);
                let Some(end) = a1.checked_add(len) else {
                    return fail(ENOMEM);
                };
                let reserved = self
                    .proc(i)
                    .vmas
                    .iter()
                    .any(|&(s, l)| a1 >= s && end <= s + l);
                if reserved {
                    let lazy = &mut self.proc(i).linux_lazy;
                    lazy_cut(lazy, a1, len);
                    if a3 != 0 {
                        lazy.push((a1, len, (a3 & 7) | zoo_abi::prot::READ));
                    }
                }
                if a3 != 0 {
                    let (w, x) = (a3 & 2 != 0, a3 & 4 != 0);
                    let mut pages = 0;
                    let mut va = a1;
                    while let Some(m) = mem.next_mapped(p4, va, end) {
                        if mem.protect_user_page(p4, m, w, x).is_err() {
                            return fail(ENOMEM);
                        }
                        pages += 1;
                        va = m + 4096;
                    }
                    // Outside a reservation every page must be committed.
                    if !reserved && pages != len / 4096 {
                        return fail(ENOMEM);
                    }
                }
                Outcome::Ret(0)
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
            // ---- files and folders ----
            // The home folder (/home/user) is BadgerHandler's, read and
            // written through it (docs/linux4-design.md); everything else is
            // the read-only initrd (docs/linux2-design.md, linux3).
            // open(path, flags), openat(dirfd, path, flags)
            2 | 257 => {
                let (path, flags) = if number == 2 { (a1, a2) } else { (a2, a3) };
                let Some(path) = self.linux_path(i, &ok, path) else {
                    return fail(EFAULT);
                };
                let write = flags & 3 != 0;
                let file = if let Some(rel) = self.home_rel(i, &path) {
                    let (status, entry) = step!(self.fs(i, Op::Stat, 0, 0, &rel, &[]));
                    let exists = status >= 0;
                    if !exists && status != Error::NotFound.as_return() {
                        return fail(zoo_errno(status));
                    }
                    let is_dir = exists && entry.first() == Some(&(zoo_abi::KIND_DIR as u8));
                    if !exists {
                        if flags & O_CREAT == 0 {
                            return fail(ENOENT);
                        }
                        let (s, _) = step!(self.fs(i, Op::CreateFile, 0, 0, &rel, &[]));
                        if s < 0 {
                            return fail(zoo_errno(s));
                        }
                    } else if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                        return fail(EEXIST);
                    }
                    if is_dir {
                        if write {
                            return fail(EISDIR);
                        }
                        LinuxFile::HomeDir(rel, 0)
                    } else {
                        if flags & O_DIRECTORY != 0 {
                            return fail(ENOTDIR);
                        }
                        if exists && write && flags & O_TRUNC != 0 {
                            let (s, _) = step!(self.fs(i, Op::Truncate, 0, 0, &rel, &[]));
                            if s < 0 {
                                return fail(zoo_errno(s));
                            }
                        }
                        LinuxFile::Home {
                            path: rel,
                            pos: 0,
                            write,
                            append: flags & O_APPEND != 0,
                        }
                    }
                } else {
                    if write || flags & O_CREAT != 0 {
                        return fail(EROFS);
                    }
                    match initrd_kind(&path) {
                        Some(Kind::File(_)) if flags & O_DIRECTORY != 0 => return fail(ENOTDIR),
                        Some(Kind::File(data)) => LinuxFile::File(data, 0),
                        Some(Kind::Dir) => LinuxFile::Dir(path, 0),
                        None => return fail(ENOENT),
                    }
                };
                match self.alloc_fd(i, 0, file, flags & O_CLOEXEC != 0) {
                    Ok(fd) => {
                        self.proc(i).set_nonblock(fd, flags & O_NONBLOCK != 0);
                        Outcome::Ret(fd as i64)
                    }
                    Err(e) => fail(e),
                }
            }
            // close(fd)
            3 => {
                let p = self.proc(i);
                match p.linux_files.get_mut(a1 as usize) {
                    Some(slot @ Some(_)) => {
                        *slot = None;
                        p.set_cloexec(a1 as usize, false);
                        p.set_nonblock(a1 as usize, false);
                        // A pipe's reader may now see its end.
                        self.wake_all();
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
                let (data, pos) = match self.linux_file(i, a1).cloned() {
                    // Standard input: at its end (no terminal yet).
                    Some(LinuxFile::Console { input: true }) => return Outcome::Ret(0),
                    Some(LinuxFile::Pipe(end)) if !end.write => {
                        return self.pipe_read(i, &end.pipe, &pieces, mem, usize::MAX);
                    }
                    // Descriptors sent along are dropped by a plain read.
                    Some(LinuxFile::Sock(s)) => return self.sock_recv(i, &s, &pieces, mem).0,
                    Some(LinuxFile::Event(e)) => {
                        let Some(&(base, len)) = pieces.iter().find(|p| p.1 > 0) else {
                            return fail(EINVAL);
                        };
                        if len < 8 {
                            return fail(EINVAL);
                        }
                        if !ok(base, 8, true) {
                            return fail(EFAULT);
                        }
                        let v = {
                            let mut ev = e.lock();
                            if ev.count == 0 {
                                return Outcome::Block;
                            }
                            let v = if ev.semaphore { 1 } else { ev.count };
                            ev.count -= v;
                            v
                        };
                        // SAFETY: checked as writable user memory.
                        unsafe { (base as *mut u64).write_unaligned(v) };
                        self.wake_all();
                        return Outcome::Ret(8);
                    }
                    Some(LinuxFile::Console { .. } | LinuxFile::Pipe(_) | LinuxFile::Epoll(_)) => {
                        return fail(EINVAL);
                    }
                    Some(LinuxFile::File(data, pos)) => (data, pos),
                    Some(LinuxFile::Home { path, pos, .. }) => {
                        // One request: the first piece, up to the shared
                        // buffer (or a message without one).
                        let Some(&(base, len)) = pieces.iter().find(|p| p.1 > 0) else {
                            return Outcome::Ret(0);
                        };
                        let n = if let Some(shm) = self.proc(i).linux_shm.clone() {
                            let want = (len as usize).min(LINUX_BULK);
                            let len32 = (want as u32).to_le_bytes();
                            let (s, _) = step!(self.fs(i, Op::ReadBulk, pos, 0, &path, &len32));
                            if s < 0 {
                                return fail(zoo_errno(s));
                            }
                            let n = (s as usize).min(want);
                            if n > 0 {
                                if !ok(base, n as u64, true) {
                                    return fail(EFAULT);
                                }
                                shm_to_user(mem, &shm, base, n);
                            }
                            n
                        } else {
                            let want = (len as usize).min(FS_MAX_DATA);
                            let (s, bytes) =
                                step!(self.fs(i, Op::Read, pos, want as u16, &path, &[]));
                            if s < 0 {
                                return fail(zoo_errno(s));
                            }
                            let n = bytes.len().min(want);
                            if n > 0 {
                                if !ok(base, n as u64, true) {
                                    return fail(EFAULT);
                                }
                                // SAFETY: checked as writable user memory.
                                unsafe {
                                    core::ptr::copy_nonoverlapping(
                                        bytes.as_ptr(),
                                        base as *mut u8,
                                        n,
                                    )
                                };
                            }
                            n
                        };
                        if let Some(LinuxFile::Home { pos, .. }) = self.linux_file(i, a1) {
                            *pos += n as u64;
                        }
                        return Outcome::Ret(n as i64);
                    }
                    Some(LinuxFile::Dir(..) | LinuxFile::HomeDir(..)) => return fail(EISDIR),
                    None => return fail(EBADF),
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
                if let Some(LinuxFile::File(_, pos)) = self.linux_file(i, a1) {
                    *pos = at as u64;
                }
                Outcome::Ret(total as i64)
            }
            // lseek(fd, offset, whence); on a folder only a rewind.
            8 => {
                let (len, pos) = match self.linux_file(i, a1).cloned() {
                    Some(LinuxFile::File(data, pos)) => (data.len() as i64, pos as i64),
                    Some(LinuxFile::Home { path, pos, .. }) => {
                        let len = if a3 == 2 {
                            let (s, entry) = step!(self.fs(i, Op::Stat, 0, 0, &path, &[]));
                            if s < 0 {
                                return fail(zoo_errno(s));
                            }
                            entry_size(&entry) as i64
                        } else {
                            0
                        };
                        (len, pos as i64)
                    }
                    Some(LinuxFile::Dir(..) | LinuxFile::HomeDir(..)) if a2 == 0 && a3 == 0 => {
                        if let Some(LinuxFile::Dir(_, pos) | LinuxFile::HomeDir(_, pos)) =
                            self.linux_file(i, a1)
                        {
                            *pos = 0;
                        }
                        return Outcome::Ret(0);
                    }
                    Some(LinuxFile::Dir(..) | LinuxFile::HomeDir(..)) => return fail(EINVAL),
                    Some(
                        LinuxFile::Console { .. }
                        | LinuxFile::Pipe(_)
                        | LinuxFile::Sock(_)
                        | LinuxFile::Event(_)
                        | LinuxFile::Epoll(_),
                    ) => return fail(ESPIPE),
                    None => return fail(EBADF),
                };
                let from = match a3 {
                    0 => 0i64,
                    1 => pos,
                    2 => len,
                    _ => return fail(EINVAL),
                };
                let new = from.saturating_add(a2 as i64);
                if new < 0 {
                    return fail(EINVAL);
                }
                match self.linux_file(i, a1) {
                    Some(LinuxFile::File(_, pos)) | Some(LinuxFile::Home { pos, .. }) => {
                        *pos = new as u64
                    }
                    _ => {}
                }
                Outcome::Ret(new)
            }
            // getdents64(fd, buf, count): a folder's entries, `.` and `..`
            // first.
            217 => {
                let (home, dir, start) = match self.linux_file(i, a1).cloned() {
                    Some(LinuxFile::Dir(path, pos)) => (false, path, pos),
                    Some(LinuxFile::HomeDir(path, pos)) => (true, path, pos),
                    Some(_) => return fail(ENOTDIR),
                    None => return fail(EBADF),
                };
                let mut out = alloc::vec::Vec::new();
                let mut next = start;
                let mut k = start;
                loop {
                    // Entry k: `.`, `..`, then the folder's own entries.
                    let (name, is_dir) = match k {
                        0 => (String::from("."), true),
                        1 => (String::from(".."), true),
                        _ if home => {
                            let (s, entry) =
                                step!(self.fs(i, Op::List, (k - 2) as u64, 0, &dir, &[]));
                            if s < 0 {
                                break; // past the end
                            }
                            let name =
                                String::from_utf8_lossy(entry.get(9..).unwrap_or(&[])).into_owned();
                            (name, entry.first() == Some(&(zoo_abi::KIND_DIR as u8)))
                        }
                        _ => match initrd_children(&dir).into_iter().nth(k - 2) {
                            Some(e) => e,
                            None => break,
                        },
                    };
                    // struct linux_dirent64: ino, off, reclen, type, name.
                    let reclen = (19 + name.len() + 1).div_ceil(8) * 8;
                    if out.len() + reclen > a3 as usize {
                        if out.is_empty() {
                            return fail(EINVAL); // too small for one entry
                        }
                        break;
                    }
                    let mut rec = alloc::vec![0u8; reclen];
                    rec[0..8].copy_from_slice(&(k as u64 + 1).to_le_bytes());
                    rec[8..16].copy_from_slice(&(k as u64 + 1).to_le_bytes());
                    rec[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
                    rec[18] = if is_dir { 4 } else { 8 }; // DT_DIR, DT_REG
                    rec[19..19 + name.len()].copy_from_slice(name.as_bytes());
                    out.extend_from_slice(&rec);
                    k += 1;
                    next = k;
                }
                if !out.is_empty() {
                    if !ok(a2, out.len() as u64, true) {
                        return fail(EFAULT);
                    }
                    // SAFETY: checked as writable user memory.
                    unsafe {
                        core::ptr::copy_nonoverlapping(out.as_ptr(), a2 as *mut u8, out.len())
                    };
                }
                if let Some(LinuxFile::Dir(_, pos) | LinuxFile::HomeDir(_, pos)) =
                    self.linux_file(i, a1)
                {
                    *pos = next;
                }
                Outcome::Ret(out.len() as i64)
            }
            // fstat(fd, st); stat(path, st); lstat(path, st);
            // newfstatat(dirfd, path, st, flags)
            5 | 4 | 6 | 262 => {
                // Device and inode: distinct per file (musl's loader tells
                // libraries apart by them). Initrd files use their address,
                // home-folder files a hash of their path.
                let mut ino = 0u64;
                let (fd, path, st) = match number {
                    5 => (Some(a1), 0, a2),
                    4 | 6 => (None, a1, a2),
                    _ if f.r10 & AT_EMPTY_PATH != 0 => (Some(a1), 0, a3),
                    _ => (None, a2, a3),
                };
                // A home-folder path to ask BadgerHandler about, or (mode, size).
                let (home_path, known) = match fd {
                    Some(fd) => match self.linux_file(i, fd).cloned() {
                        Some(LinuxFile::Console { .. }) => (None, Some((0o020_620u32, 0))),
                        Some(LinuxFile::Pipe(end)) => {
                            let len = end.pipe.lock().len as u64;
                            (None, Some((0o010_600, len)))
                        }
                        Some(LinuxFile::Sock(_)) => (None, Some((0o140_777, 0))),
                        Some(LinuxFile::Event(_) | LinuxFile::Epoll(_)) => (None, Some((0o600, 0))),
                        Some(LinuxFile::File(data, _)) => {
                            ino = data.as_ptr() as u64;
                            (None, Some((0o100_444, data.len() as u64)))
                        }
                        Some(LinuxFile::Dir(..)) => (None, Some((0o040_555, 0))),
                        Some(LinuxFile::Home { path, .. } | LinuxFile::HomeDir(path, _)) => {
                            (Some(path), None)
                        }
                        None => return fail(EBADF),
                    },
                    None => {
                        let Some(path) = self.linux_path(i, &ok, path) else {
                            return fail(EFAULT);
                        };
                        if let Some(rel) = self.home_rel(i, &path) {
                            (Some(rel), None)
                        } else {
                            match initrd_kind(&path) {
                                Some(Kind::File(data)) => {
                                    ino = data.as_ptr() as u64;
                                    (None, Some((0o100_444, data.len() as u64)))
                                }
                                Some(Kind::Dir) => (None, Some((0o040_555, 0))),
                                None => return fail(ENOENT),
                            }
                        }
                    }
                };
                let (mode, len) = match (home_path, known) {
                    (_, Some(k)) => k,
                    (Some(rel), None) => {
                        ino = rel.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                            (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
                        });
                        let (s, entry) = step!(self.fs(i, Op::Stat, 0, 0, &rel, &[]));
                        if s < 0 {
                            return fail(zoo_errno(s));
                        }
                        if entry.first() == Some(&(zoo_abi::KIND_DIR as u8)) {
                            (0o040_755, 0)
                        } else {
                            (0o100_644, entry_size(&entry))
                        }
                    }
                    (None, None) => return fail(EBADF),
                };
                if !ok(st, 144, true) {
                    return fail(EFAULT);
                }
                // struct stat (x86-64): nlink at 16, mode at 24, uid and gid
                // at 28 and 32, size at 48, blksize at 56, blocks at 64.
                let mut buf = [0u8; 144];
                buf[0..8].copy_from_slice(&1u64.to_le_bytes());
                buf[8..16].copy_from_slice(&ino.to_le_bytes());
                buf[16..24].copy_from_slice(&1u64.to_le_bytes());
                buf[24..28].copy_from_slice(&mode.to_le_bytes());
                buf[28..32].copy_from_slice(&LINUX_UID.to_le_bytes());
                buf[32..36].copy_from_slice(&LINUX_UID.to_le_bytes());
                buf[48..56].copy_from_slice(&len.to_le_bytes());
                buf[56..64].copy_from_slice(&4096u64.to_le_bytes());
                buf[64..72].copy_from_slice(&len.div_ceil(512).to_le_bytes());
                // SAFETY: checked as writable user memory.
                unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), st as *mut u8, 144) };
                Outcome::Ret(0)
            }
            // access(path, mode), faccessat(dirfd, path, mode)
            21 | 269 => {
                let (path, mode) = if number == 21 { (a1, a2) } else { (a2, a3) };
                let Some(path) = self.linux_path(i, &ok, path) else {
                    return fail(EFAULT);
                };
                if let Some(rel) = self.home_rel(i, &path) {
                    let (s, _) = step!(self.fs(i, Op::Stat, 0, 0, &rel, &[]));
                    return if s < 0 {
                        fail(zoo_errno(s))
                    } else {
                        Outcome::Ret(0)
                    };
                }
                match initrd_kind(&path) {
                    None => fail(ENOENT),
                    Some(_) if mode & 2 != 0 => fail(EROFS), // W_OK
                    Some(_) => Outcome::Ret(0),
                }
            }
            // getcwd(buf, size): the length written, NUL included.
            79 => {
                let cwd = alloc::format!("/{}", self.proc(i).linux_cwd);
                let n = cwd.len() + 1;
                if (a2 as usize) < n {
                    return fail(ERANGE);
                }
                if !ok(a1, n as u64, true) {
                    return fail(EFAULT);
                }
                // SAFETY: checked as writable user memory.
                unsafe {
                    core::ptr::copy_nonoverlapping(cwd.as_ptr(), a1 as *mut u8, cwd.len());
                    (a1 as *mut u8).add(cwd.len()).write(0);
                }
                Outcome::Ret(n as i64)
            }
            // chdir(path)
            80 => {
                let Some(path) = self.linux_path(i, &ok, a1) else {
                    return fail(EFAULT);
                };
                let is_dir = if let Some(rel) = self.home_rel(i, &path) {
                    let (s, entry) = step!(self.fs(i, Op::Stat, 0, 0, &rel, &[]));
                    if s < 0 {
                        return fail(zoo_errno(s));
                    }
                    entry.first() == Some(&(zoo_abi::KIND_DIR as u8))
                } else {
                    match initrd_kind(&path) {
                        Some(Kind::Dir) => true,
                        Some(Kind::File(_)) => false,
                        None => return fail(ENOENT),
                    }
                };
                if !is_dir {
                    return fail(ENOTDIR);
                }
                self.proc(i).linux_cwd = path;
                Outcome::Ret(0)
            }
            // mkdir(path, mode), mkdirat(dirfd, path, mode), rmdir(path),
            // unlink(path), unlinkat(dirfd, path, flags): home folder only.
            83 | 258 | 84 | 87 | 263 => {
                let path = match number {
                    258 | 263 => a2,
                    _ => a1,
                };
                let op = match number {
                    83 | 258 => Op::CreateDir,
                    84 => Op::RemoveDir,
                    263 if a3 & AT_REMOVEDIR != 0 => Op::RemoveDir,
                    _ => Op::RemoveFile,
                };
                let Some(path) = self.linux_path(i, &ok, path) else {
                    return fail(EFAULT);
                };
                let Some(rel) = self.home_rel(i, &path) else {
                    return fail(if initrd_kind(&path).is_some() || op == Op::CreateDir {
                        EROFS
                    } else {
                        ENOENT
                    });
                };
                let (s, _) = step!(self.fs(i, op, 0, 0, &rel, &[]));
                if s < 0 {
                    fail(zoo_errno(s))
                } else {
                    Outcome::Ret(0)
                }
            }
            // rename(old, new), renameat(olddir, old, newdir, new),
            // renameat2(…, flags): within the home folder.
            82 | 264 | 316 => {
                let (from, to) = if number == 82 { (a1, a2) } else { (a2, f.r10) };
                let (Some(from), Some(to)) =
                    (self.linux_path(i, &ok, from), self.linux_path(i, &ok, to))
                else {
                    return fail(EFAULT);
                };
                let (Some(from), Some(to)) = (self.home_rel(i, &from), self.home_rel(i, &to))
                else {
                    return fail(EXDEV);
                };
                let (s, _) = step!(self.fs(i, Op::Rename, 0, 0, &from, to.as_bytes()));
                if s < 0 {
                    fail(zoo_errno(s))
                } else {
                    Outcome::Ret(0)
                }
            }
            // ftruncate(fd, len), truncate(path, len)
            77 | 76 => {
                let path = if number == 77 {
                    match self.linux_file(i, a1).cloned() {
                        Some(LinuxFile::Home {
                            path, write: true, ..
                        }) => path,
                        Some(_) => return fail(EINVAL),
                        None => return fail(EBADF),
                    }
                } else {
                    let Some(path) = self.linux_path(i, &ok, a1) else {
                        return fail(EFAULT);
                    };
                    match self.home_rel(i, &path) {
                        Some(rel) => rel,
                        None => return fail(EROFS),
                    }
                };
                let (s, _) = step!(self.fs(i, Op::Truncate, a2, 0, &path, &[]));
                if s < 0 {
                    fail(zoo_errno(s))
                } else {
                    Outcome::Ret(0)
                }
            }
            // fsync, fdatasync: BadgerHandler saves on its own schedule.
            74 | 75 => Outcome::Ret(0),
            // uname(buf): six 65-byte fields. The system reports itself as
            // Linux, as other kernels' Linux layers do, so programs that
            // check take their Linux paths (docs/linux3-design.md).
            63 => {
                if !ok(a1, 6 * 65, true) {
                    return fail(EFAULT);
                }
                let fields = [
                    "Linux",
                    "zoo",
                    "6.1.0-zooos",
                    concat!("ZooOS ", env!("CARGO_PKG_VERSION")),
                    "x86_64",
                    "(none)",
                ];
                let mut buf = [0u8; 6 * 65];
                for (k, v) in fields.iter().enumerate() {
                    buf[k * 65..k * 65 + v.len()].copy_from_slice(v.as_bytes());
                }
                // SAFETY: checked as writable user memory.
                unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), a1 as *mut u8, buf.len()) };
                Outcome::Ret(0)
            }
            // getuid, getgid, geteuid, getegid: one ordinary user.
            102 | 104 | 107 | 108 => Outcome::Ret(i64::from(LINUX_UID)),
            // getppid: 0 when started by the kernel.
            110 => Outcome::Ret(self.proc(i).linux_parent as i64),
            // fcntl(fd, cmd, arg): duplicate, close-on-exec, status flags
            // (setting them is accepted and has no effect yet).
            72 => {
                let Some(file) = self.linux_file(i, a1).cloned() else {
                    return fail(EBADF);
                };
                let fd = a1 as usize;
                match a2 {
                    // F_DUPFD, F_DUPFD_CLOEXEC
                    0 | 1030 => match self.alloc_fd(i, a3 as usize, file, a2 == 1030) {
                        Ok(new) => {
                            let nb = self.proc(i).nonblock(a1);
                            self.proc(i).set_nonblock(new, nb);
                            Outcome::Ret(new as i64)
                        }
                        Err(e) => fail(e),
                    },
                    // F_GETFD, F_SETFD
                    1 => Outcome::Ret((self.proc(i).linux_cloexec >> fd & 1) as i64),
                    2 => {
                        self.proc(i).set_cloexec(fd, a3 & 1 != 0);
                        Outcome::Ret(0)
                    }
                    // F_GETFL: the access mode, append and non-blocking.
                    3 => {
                        let nb = if self.proc(i).nonblock(a1) {
                            O_NONBLOCK as i64
                        } else {
                            0
                        };
                        Outcome::Ret(
                            nb | match file {
                                LinuxFile::Pipe(end) if end.write => 1,
                                LinuxFile::Console { input: false } => 1,
                                LinuxFile::Sock(_) | LinuxFile::Event(_) => 2,
                                LinuxFile::Home {
                                    write: true,
                                    append,
                                    ..
                                } => 1 | if append { O_APPEND as i64 } else { 0 },
                                _ => 0,
                            },
                        )
                    }
                    // F_SETFL: only O_NONBLOCK has an effect.
                    4 => {
                        self.proc(i).set_nonblock(fd, a3 & O_NONBLOCK != 0);
                        Outcome::Ret(0)
                    }
                    _ => fail(EINVAL),
                }
            }
            // ---- processes, pipes, signals (docs/linux5-design.md) ----
            // fork(), vfork(), clone(flags, stack, …)
            57 => self.linux_fork(i, f, mem, false, 0),
            58 => self.linux_fork(i, f, mem, true, 0),
            56 => match a1 & !0xff {
                0 => self.linux_fork(i, f, mem, false, a2),
                x if x == CLONE_VM | CLONE_VFORK => self.linux_fork(i, f, mem, true, a2),
                x if x & THREAD_NEEDS == THREAD_NEEDS && x & !(THREAD_NEEDS | THREAD_MAY) == 0 => {
                    let (ctid, tls) = (f.r10, f.r8);
                    self.linux_thread(i, f, mem, x, a2, a3, ctid, tls)
                }
                _ => {
                    kprintln!("ZOO_OS INFO linux clone flags={:#x} unsupported", a1);
                    fail(ENOSYS)
                }
            },
            // execve(path, argv, envp)
            59 => {
                let Some(path) = self.linux_path(i, &ok, a1) else {
                    return fail(EFAULT);
                };
                let mut lists = [Vec::new(), Vec::new()];
                for (list, addr) in lists.iter_mut().zip([a2, a3]) {
                    if addr == 0 {
                        continue;
                    }
                    for k in 0..=LINUX_ARGS_MAX as u64 {
                        let at = addr + 8 * k;
                        if !ok(at, 8, false) {
                            return fail(EFAULT);
                        }
                        // SAFETY: checked as readable user memory.
                        let p = unsafe { (at as *const u64).read_unaligned() };
                        if p == 0 {
                            break;
                        }
                        if k == LINUX_ARGS_MAX as u64 {
                            return fail(E2BIG);
                        }
                        let mut buf = [0u8; LINUX_ARG_MAX];
                        let Some(n) = user_cstr(&ok, p, &mut buf) else {
                            return fail(E2BIG);
                        };
                        let Ok(s) = core::str::from_utf8(&buf[..n]) else {
                            return fail(EINVAL);
                        };
                        list.push(String::from(s));
                    }
                }
                let [argv, env] = lists;
                self.linux_exec(i, f, mem, &path, argv, env)
            }
            // wait4(pid, status, options, rusage): pid > 0 one child, else
            // any (there is one process group).
            61 => {
                let (want, st, nohang, ru) = (a1 as i64, a2, a3 & 1 != 0, f.r10);
                if (st != 0 && !ok(st, 4, true)) || (ru != 0 && !ok(ru, 144, true)) {
                    return fail(EFAULT);
                }
                let kids = &mut self.proc(i).linux_children;
                let fits = |pid: u64| want <= 0 || pid == want as u64;
                if !kids.iter().any(|c| fits(c.0)) {
                    return fail(ECHILD);
                }
                if let Some(k) = kids.iter().position(|c| fits(c.0) && c.1.is_some()) {
                    let (pid, status) = kids.remove(k);
                    // SAFETY: both checked as writable user memory.
                    unsafe {
                        if st != 0 {
                            (st as *mut i32).write_unaligned(status.unwrap_or(0));
                        }
                        if ru != 0 {
                            core::ptr::write_bytes(ru as *mut u8, 0, 144);
                        }
                    }
                    return Outcome::Ret(pid as i64);
                }
                if nohang {
                    Outcome::Ret(0)
                } else {
                    Outcome::Block
                }
            }
            // pipe(fds), pipe2(fds, flags)
            22 | 293 => {
                let flags = if number == 293 { a2 } else { 0 };
                if flags & !(O_CLOEXEC | O_NONBLOCK) != 0 {
                    return fail(EINVAL);
                }
                if !ok(a1, 8, true) {
                    return fail(EFAULT);
                }
                let Some(pipe) = Pipe::new(mem) else {
                    return fail(ENOMEM);
                };
                let pipe = Arc::new(Mutex::new(pipe));
                let reader = LinuxFile::Pipe(PipeEnd::new(pipe.clone(), false));
                let writer = LinuxFile::Pipe(PipeEnd::new(pipe, true));
                let cloexec = flags & O_CLOEXEC != 0;
                let r = match self.alloc_fd(i, 0, reader, cloexec) {
                    Ok(fd) => fd,
                    Err(e) => return fail(e),
                };
                let w = match self.alloc_fd(i, 0, writer, cloexec) {
                    Ok(fd) => fd,
                    Err(e) => {
                        self.proc(i).linux_files[r] = None;
                        return fail(e);
                    }
                };
                let nb = flags & O_NONBLOCK != 0;
                self.proc(i).set_nonblock(r, nb);
                self.proc(i).set_nonblock(w, nb);
                // SAFETY: checked as writable user memory (two ints).
                unsafe {
                    (a1 as *mut i32).write_unaligned(r as i32);
                    (a1 as *mut i32).add(1).write_unaligned(w as i32);
                }
                Outcome::Ret(0)
            }
            // dup(fd)
            32 => match self.linux_file(i, a1).cloned() {
                Some(file) => match self.alloc_fd(i, 0, file, false) {
                    Ok(fd) => {
                        let nb = self.proc(i).nonblock(a1);
                        self.proc(i).set_nonblock(fd, nb);
                        Outcome::Ret(fd as i64)
                    }
                    Err(e) => fail(e),
                },
                None => fail(EBADF),
            },
            // dup2(old, new), dup3(old, new, flags)
            33 | 292 => {
                let flags = if number == 292 { a3 } else { 0 };
                if flags & !O_CLOEXEC != 0 || (number == 292 && a1 == a2) {
                    return fail(EINVAL);
                }
                let Some(file) = self.linux_file(i, a1).cloned() else {
                    return fail(EBADF);
                };
                let fd = a2 as usize;
                if fd >= LINUX_FILES_MAX {
                    return fail(EBADF);
                }
                if a1 == a2 {
                    return Outcome::Ret(fd as i64);
                }
                let p = self.proc(i);
                while p.linux_files.len() <= fd {
                    p.linux_files.push(None);
                }
                // What `new` had open closes.
                p.linux_files[fd] = Some(file);
                p.set_cloexec(fd, flags & O_CLOEXEC != 0);
                let nb = p.nonblock(a1);
                p.set_nonblock(fd, nb);
                self.wake_all();
                Outcome::Ret(fd as i64)
            }
            // kill(pid, sig), tkill(tid, sig), tgkill(tgid, tid, sig): to
            // Linux programs only; pid 0 is the caller (one process group).
            62 | 200 | 234 => {
                let (id, sig) = if number == 234 { (a2, a3) } else { (a1, a2) };
                if sig > 64 {
                    return fail(EINVAL);
                }
                let me = self.task(i).pid;
                let k = if number == 62 {
                    let target = match id as i64 {
                        0 => me,
                        p if p > 0 => p as u64,
                        _ => return fail(ESRCH),
                    };
                    self.task_of_pid(target)
                } else {
                    // A thread: tkill(tid), tgkill(tgid, tid).
                    self.tasks.iter().position(|t| {
                        matches!(t, Some(t) if t.tid == id && t.proc.is_some()
                            && t.state != State::Exited && (number != 234 || t.pid == a1))
                    })
                };
                let Some(k) = k else {
                    return fail(ESRCH);
                };
                if !self.proc(k).linux {
                    return fail(EPERM);
                }
                if sig == 0 {
                    return Outcome::Ret(0);
                }
                self.linux_send(k, sig, me, number != 62);
                if self.proc(i).ended {
                    Outcome::Exit(0)
                } else {
                    Outcome::Ret(0)
                }
            }
            // rt_sigaction(sig, act, oldact, size) (docs/linux6-design.md).
            13 => {
                let (sig, act, old) = (a1, a2, a3);
                if !(1..=64).contains(&sig) || (act != 0 && (sig == SIGKILL || sig == SIGSTOP)) {
                    return fail(EINVAL);
                }
                if (act != 0 && !ok(act, 32, false)) || (old != 0 && !ok(old, 32, true)) {
                    return fail(EFAULT);
                }
                let mut new = [0u64; 4];
                if act != 0 {
                    for (k, v) in new.iter_mut().enumerate() {
                        // SAFETY: checked as readable user memory.
                        *v = unsafe { (act as *const u64).add(k).read_unaligned() };
                    }
                }
                let list = &mut self.proc(i).linux_sigactions;
                if old != 0 {
                    let cur = list
                        .iter()
                        .find(|a| u64::from(a.0) == sig)
                        .map_or([0u64; 4], |a| a.1);
                    for (k, v) in cur.iter().enumerate() {
                        // SAFETY: checked as writable user memory.
                        unsafe { (old as *mut u64).add(k).write_unaligned(*v) };
                    }
                }
                if act != 0 {
                    list.retain(|a| u64::from(a.0) != sig);
                    if new[0] != 0 {
                        list.push((sig as u8, new));
                    }
                }
                Outcome::Ret(0)
            }
            // rt_sigprocmask(how, set, oldset, size)
            14 => {
                let (how, set, old) = (a1, a2, a3);
                if (set != 0 && !ok(set, 8, false)) || (old != 0 && !ok(old, 8, true)) {
                    return fail(EFAULT);
                }
                let cur = self.task(i).sigmask;
                let new = if set == 0 {
                    cur
                } else {
                    // SAFETY: checked as readable user memory.
                    let s = unsafe { (set as *const u64).read_unaligned() };
                    match how {
                        0 => cur | s,
                        1 => cur & !s,
                        2 => s,
                        _ => return fail(EINVAL),
                    }
                };
                if old != 0 {
                    // SAFETY: checked as writable user memory.
                    unsafe { (old as *mut u64).write_unaligned(cur) };
                }
                // SIGKILL and SIGSTOP cannot be blocked.
                self.task(i).sigmask = new & !UNBLOCKABLE;
                Outcome::Ret(0)
            }
            // rt_sigreturn: back from a handler, restoring what its frame
            // saved (possibly edited by the handler). A bad frame ends the
            // program with SIGSEGV.
            15 => {
                let uc = f.rsp; // the handler's `ret` popped the return address
                let bad = |s: &mut Self| {
                    s.proc(i).linux_term_sig = Some(SIGSEGV as u8);
                    Outcome::Crash
                };
                if !ok(uc, 304, false) {
                    return bad(self);
                }
                let mut g = [0u64; 24];
                for (k, v) in g.iter_mut().enumerate() {
                    // SAFETY: inside the checked readable `ucontext`.
                    *v = unsafe { ((uc + 40) as *const u64).add(k).read_unaligned() };
                }
                // SAFETY: inside the checked readable `ucontext`.
                let mask = unsafe { ((uc + 296) as *const u64).read_unaligned() };
                if g[16] >= USER_HALF_END || g[15] >= USER_HALF_END {
                    return bad(self);
                }
                let fx = if g[23] != 0 {
                    if !ok(g[23], 512, false) {
                        return bad(self);
                    }
                    // SAFETY: checked as 512 readable user bytes.
                    let bytes = unsafe { (g[23] as *const [u8; 512]).read_unaligned() };
                    match fpu::from_user(&bytes) {
                        Some(fx) => Some(fx),
                        None => return bad(self),
                    }
                } else {
                    None
                };
                f.r8 = g[0];
                f.r9 = g[1];
                f.r10 = g[2];
                f.r11 = g[3];
                f.r12 = g[4];
                f.r13 = g[5];
                f.r14 = g[6];
                f.r15 = g[7];
                f.rdi = g[8];
                f.rsi = g[9];
                f.rbp = g[10];
                f.rbx = g[11];
                f.rdx = g[12];
                f.rax = g[13];
                f.rcx = g[14];
                f.rsp = g[15];
                f.rip = g[16];
                // Only arithmetic and direction flags come from the program.
                f.rflags = (g[17] & LINUX_USER_FLAGS) | RFLAGS_IF | RFLAGS_RESERVED;
                if let Some(fx) = fx {
                    fpu::restore(&fx);
                }
                self.task(i).sigmask = mask & !UNBLOCKABLE;
                Outcome::Resume
            }
            // rt_sigpending(set, size): pending signals this thread blocks.
            127 => {
                if !ok(a1, 8, true) {
                    return fail(EFAULT);
                }
                let t = self.task(i);
                let (pending, mask) = (t.sig_pending, t.sigmask);
                let set = (pending | self.proc(i).linux_pending) & mask;
                // SAFETY: checked as writable user memory.
                unsafe { (a1 as *mut u64).write_unaligned(set) };
                Outcome::Ret(0)
            }
            // sigaltstack(ss, old_ss)
            131 => {
                let (ss, old) = (a1, a2);
                if (ss != 0 && !ok(ss, 24, false)) || (old != 0 && !ok(old, 24, true)) {
                    return fail(EFAULT);
                }
                let (sp, size, on) = self.task(i).altstack;
                let on_alt = on && f.rsp.wrapping_sub(sp) < size;
                if old != 0 {
                    let flags = if on_alt {
                        SS_ONSTACK
                    } else if on {
                        0
                    } else {
                        SS_DISABLE
                    };
                    // SAFETY: checked as writable user memory (24 bytes).
                    unsafe {
                        let p = old as *mut u64;
                        p.write_unaligned(sp);
                        p.add(1).write_unaligned(flags);
                        p.add(2).write_unaligned(size);
                    }
                }
                if ss != 0 {
                    // SAFETY: checked as readable user memory (24 bytes).
                    let (new_sp, new_flags, new_size) = unsafe {
                        let p = ss as *const u64;
                        (
                            p.read_unaligned(),
                            p.add(1).read_unaligned() & 0xFFFF_FFFF,
                            p.add(2).read_unaligned(),
                        )
                    };
                    if on_alt {
                        return fail(EPERM);
                    }
                    if new_flags & SS_DISABLE != 0 {
                        self.task(i).altstack = (0, 0, false);
                    } else if new_flags & !SS_AUTODISARM != 0 {
                        return fail(EINVAL);
                    } else if new_size < 2048 {
                        return fail(ENOMEM);
                    } else {
                        self.task(i).altstack = (new_sp, new_size, true);
                    }
                }
                Outcome::Ret(0)
            }
            // pause(): until a signal is handled (EINTR).
            34 => Outcome::Block,
            // ---- sockets, events and waits (docs/linux7-design.md) ----
            // socket(domain, type, protocol): Unix stream sockets.
            41 => {
                if a1 != AF_UNIX {
                    return fail(EAFNOSUPPORT);
                }
                if a2 & 0xF != SOCK_STREAM {
                    return fail(ESOCKTNOSUPPORT);
                }
                let file = LinuxFile::Sock(Arc::new(Mutex::new(Sock::Fresh)));
                match self.alloc_fd(i, 0, file, a2 & O_CLOEXEC != 0) {
                    Ok(fd) => {
                        self.proc(i).set_nonblock(fd, a2 & O_NONBLOCK != 0);
                        Outcome::Ret(fd as i64)
                    }
                    Err(e) => fail(e),
                }
            }
            // socketpair(domain, type, protocol, sv)
            53 => {
                if a1 != AF_UNIX {
                    return fail(EAFNOSUPPORT);
                }
                if a2 & 0xF != SOCK_STREAM {
                    return fail(ESOCKTNOSUPPORT);
                }
                if !ok(f.r10, 8, true) {
                    return fail(EFAULT);
                }
                let (Some(a), Some(b)) = (Pipe::new(mem), Pipe::new(mem)) else {
                    return fail(ENOMEM);
                };
                let (a, b) = (Arc::new(Mutex::new(a)), Arc::new(Mutex::new(b)));
                let me = self.task(i).pid;
                let end = |rx: &Arc<Mutex<Pipe>>, tx: &Arc<Mutex<Pipe>>| {
                    LinuxFile::Sock(Arc::new(Mutex::new(Sock::Connected {
                        rx: PipeEnd::new(rx.clone(), false),
                        tx: Some(PipeEnd::new(tx.clone(), true)),
                        peer_pid: me,
                        local: String::new(),
                        peer: String::new(),
                    })))
                };
                let (x, y) = (end(&a, &b), end(&b, &a));
                let cloexec = a2 & O_CLOEXEC != 0;
                let fx = match self.alloc_fd(i, 0, x, cloexec) {
                    Ok(fd) => fd,
                    Err(e) => return fail(e),
                };
                let fy = match self.alloc_fd(i, 0, y, cloexec) {
                    Ok(fd) => fd,
                    Err(e) => {
                        self.proc(i).linux_files[fx] = None;
                        return fail(e);
                    }
                };
                let nb = a2 & O_NONBLOCK != 0;
                self.proc(i).set_nonblock(fx, nb);
                self.proc(i).set_nonblock(fy, nb);
                // SAFETY: checked as writable user memory (two ints).
                unsafe {
                    (f.r10 as *mut i32).write_unaligned(fx as i32);
                    (f.r10 as *mut i32).add(1).write_unaligned(fy as i32);
                }
                Outcome::Ret(0)
            }
            // bind(fd, addr, len)
            49 => {
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(if self.linux_file(i, a1).is_some() {
                        ENOTSOCK
                    } else {
                        EBADF
                    });
                };
                let cwd = self.proc(i).linux_cwd.clone();
                let name = match unix_name(&ok, a2, a3, &cwd) {
                    Ok(n) => n,
                    Err(e) => return fail(e),
                };
                self.unix_names.retain(|(_, w)| w.strong_count() > 0);
                if self.unix_names.iter().any(|(n, _)| *n == name) {
                    return fail(EADDRINUSE);
                }
                let mut st = s.lock();
                if !matches!(*st, Sock::Fresh) {
                    return fail(EINVAL);
                }
                *st = Sock::Bound(name.clone());
                drop(st);
                self.unix_names.push((name, Arc::downgrade(&s)));
                Outcome::Ret(0)
            }
            // listen(fd, backlog)
            50 => {
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(ENOTSOCK);
                };
                let me = self.task(i).pid;
                let max = (a2 as i32).clamp(1, 128) as usize;
                let mut st = s.lock();
                match &mut *st {
                    Sock::Bound(name) => {
                        let name = core::mem::take(name);
                        *st = Sock::Listening {
                            name,
                            backlog: VecDeque::new(),
                            max,
                            owner: me,
                        };
                        Outcome::Ret(0)
                    }
                    Sock::Listening { max: m, .. } => {
                        *m = max;
                        Outcome::Ret(0)
                    }
                    _ => fail(EINVAL),
                }
            }
            // accept(fd, addr, len), accept4(fd, addr, len, flags)
            43 | 288 => {
                let flags = if number == 288 { f.r10 } else { 0 };
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(ENOTSOCK);
                };
                let conn = {
                    let mut st = s.lock();
                    let Sock::Listening { name, backlog, .. } = &mut *st else {
                        return fail(EINVAL);
                    };
                    match backlog.pop_front() {
                        Some((rx, tx, peer_pid, peer)) => Sock::Connected {
                            rx,
                            tx: Some(tx),
                            peer_pid,
                            local: name.clone(),
                            peer,
                        },
                        None => return Outcome::Block,
                    }
                };
                if a2 != 0 {
                    let _ = put_unix_name(&ok, a2, a3, "");
                }
                let file = LinuxFile::Sock(Arc::new(Mutex::new(conn)));
                match self.alloc_fd(i, 0, file, flags & O_CLOEXEC != 0) {
                    Ok(fd) => {
                        self.proc(i).set_nonblock(fd, flags & O_NONBLOCK != 0);
                        Outcome::Ret(fd as i64)
                    }
                    Err(e) => fail(e),
                }
            }
            // connect(fd, addr, len)
            42 => {
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(ENOTSOCK);
                };
                let local = match &*s.lock() {
                    Sock::Fresh => String::new(),
                    Sock::Bound(n) => n.clone(),
                    Sock::Connected { .. } => return fail(EISCONN),
                    Sock::Listening { .. } => return fail(EINVAL),
                };
                let cwd = self.proc(i).linux_cwd.clone();
                let name = match unix_name(&ok, a2, a3, &cwd) {
                    Ok(n) => n,
                    Err(e) => return fail(e),
                };
                let target = self
                    .unix_names
                    .iter()
                    .find(|(n, _)| *n == name)
                    .and_then(|(_, w)| w.upgrade());
                let Some(target) = target else {
                    return fail(ECONNREFUSED);
                };
                let owner = match &*target.lock() {
                    Sock::Listening {
                        backlog,
                        max,
                        owner,
                        ..
                    } => {
                        if backlog.len() >= *max {
                            return Outcome::Block;
                        }
                        *owner
                    }
                    _ => return fail(ECONNREFUSED),
                };
                let (Some(a), Some(b)) = (Pipe::new(mem), Pipe::new(mem)) else {
                    return fail(ENOMEM);
                };
                let (a, b) = (Arc::new(Mutex::new(a)), Arc::new(Mutex::new(b)));
                let me = self.task(i).pid;
                if let Sock::Listening { backlog, .. } = &mut *target.lock() {
                    backlog.push_back((
                        PipeEnd::new(a.clone(), false),
                        PipeEnd::new(b.clone(), true),
                        me,
                        local.clone(),
                    ));
                }
                *s.lock() = Sock::Connected {
                    rx: PipeEnd::new(b, false),
                    tx: Some(PipeEnd::new(a, true)),
                    peer_pid: owner,
                    local,
                    peer: name,
                };
                self.wake_all();
                Outcome::Ret(0)
            }
            // shutdown(fd, how): writing ends for SHUT_WR and SHUT_RDWR.
            48 => {
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(ENOTSOCK);
                };
                let mut st = s.lock();
                let Sock::Connected { tx, .. } = &mut *st else {
                    return fail(ENOTCONN);
                };
                if a2 > 2 {
                    return fail(EINVAL);
                }
                if a2 != 0 {
                    let old = tx.take();
                    drop(st);
                    drop(old);
                    self.wake_all();
                }
                Outcome::Ret(0)
            }
            // getsockname(fd, addr, len), getpeername(fd, addr, len)
            51 | 52 => {
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(ENOTSOCK);
                };
                let name = match (&*s.lock(), number) {
                    (Sock::Connected { local, .. }, 51) => local.clone(),
                    (Sock::Connected { peer, .. }, _) => peer.clone(),
                    (_, 52) => return fail(ENOTCONN),
                    (Sock::Bound(n) | Sock::Listening { name: n, .. }, _) => n.clone(),
                    (Sock::Fresh, _) => String::new(),
                };
                match put_unix_name(&ok, a2, a3, &name) {
                    Ok(()) => Outcome::Ret(0),
                    Err(e) => fail(e),
                }
            }
            // setsockopt: accepted (Unix sockets have nothing to tune yet).
            54 => match self.linux_file(i, a1) {
                Some(LinuxFile::Sock(_)) => Outcome::Ret(0),
                Some(_) => fail(ENOTSOCK),
                None => fail(EBADF),
            },
            // getsockopt(fd, level, name, value, len)
            55 => {
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(ENOTSOCK);
                };
                let (val, lenp) = (f.r10, f.r8);
                if !ok(lenp, 4, true) {
                    return fail(EFAULT);
                }
                // SAFETY: checked as writable user memory.
                let room = unsafe { (lenp as *const u32).read_unaligned() } as usize;
                let mut out = [0u8; 12];
                let n = match (a2, a3) {
                    // SOL_SOCKET: SO_PEERCRED (pid, uid, gid)
                    (1, 17) => {
                        let pid = match &*s.lock() {
                            Sock::Connected { peer_pid, .. } => *peer_pid as u32,
                            _ => 0,
                        };
                        out[0..4].copy_from_slice(&pid.to_le_bytes());
                        out[4..8].copy_from_slice(&LINUX_UID.to_le_bytes());
                        out[8..12].copy_from_slice(&LINUX_UID.to_le_bytes());
                        12
                    }
                    (1, opt) => {
                        let v: i32 = match opt {
                            3 => 1,         // SO_TYPE: SOCK_STREAM
                            7 | 8 => 65536, // SO_SNDBUF, SO_RCVBUF
                            30 => i32::from(matches!(*s.lock(), Sock::Listening { .. })),
                            _ => 0, // SO_ERROR and the rest
                        };
                        out[0..4].copy_from_slice(&v.to_le_bytes());
                        4
                    }
                    _ => return fail(ENOPROTOOPT),
                };
                let n = n.min(room);
                if n > 0 && !ok(val, n as u64, true) {
                    return fail(EFAULT);
                }
                // SAFETY: both checked as writable user memory.
                unsafe {
                    core::ptr::copy_nonoverlapping(out.as_ptr(), val as *mut u8, n);
                    (lenp as *mut u32).write_unaligned(n as u32);
                }
                Outcome::Ret(0)
            }
            // sendto(fd, buf, len, flags, addr, alen): the address is
            // ignored on a connected stream socket.
            44 => {
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(ENOTSOCK);
                };
                let len = a3.min(LINUX_WRITE_MAX);
                if len > 0 && !ok(a2, len, false) {
                    return fail(EFAULT);
                }
                // SAFETY: checked as readable user memory.
                let bytes = unsafe { core::slice::from_raw_parts(a2 as *const u8, len as usize) };
                self.sock_send(i, &s, bytes, Vec::new(), f.r10 & MSG_NOSIGNAL != 0, mem)
            }
            // recvfrom(fd, buf, len, flags, addr, alen)
            45 => {
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(ENOTSOCK);
                };
                let (addr, alen) = (f.r8, f.r9);
                let (r, _) = self.sock_recv(i, &s, &[(a2, a3)], mem);
                if matches!(r, Outcome::Ret(n) if n >= 0) && addr != 0 {
                    let p4 = self.task(i).p4.expect("user task");
                    let ok2 = |a: u64, l: u64, w: bool| mem.user_range_ok(p4, a, l, w);
                    let _ = put_unix_name(&ok2, addr, alen, "");
                }
                r
            }
            // sendmsg(fd, msg, flags): data from the iovecs, descriptors
            // from an SCM_RIGHTS control message.
            46 => {
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(ENOTSOCK);
                };
                if !ok(a2, 56, false) {
                    return fail(EFAULT);
                }
                // SAFETY: checked readable `msghdr` (56 bytes).
                let m = unsafe { core::ptr::read_unaligned(a2 as *const [u64; 7]) };
                let (iov, iovlen, ctl, ctllen) = (m[2], m[3], m[4], m[5]);
                if iovlen > 1024 || (iovlen > 0 && !ok(iov, iovlen * 16, false)) {
                    return fail(EINVAL);
                }
                let mut all = Vec::new();
                for k in 0..iovlen {
                    // SAFETY: inside the checked iovec array.
                    let (base, len) = unsafe {
                        let p = (iov + 16 * k) as *const u64;
                        (p.read_unaligned(), p.add(1).read_unaligned())
                    };
                    if len == 0 {
                        continue;
                    }
                    if all.len() as u64 + len > LINUX_WRITE_MAX || !ok(base, len, false) {
                        return fail(EFAULT);
                    }
                    // SAFETY: checked as readable user memory.
                    all.extend_from_slice(unsafe {
                        core::slice::from_raw_parts(base as *const u8, len as usize)
                    });
                }
                let mut fds = Vec::new();
                if ctl != 0 && ctllen >= 16 {
                    if ctllen > 4096 || !ok(ctl, ctllen, false) {
                        return fail(EFAULT);
                    }
                    let mut at = 0u64;
                    while at + 16 <= ctllen {
                        // SAFETY: inside the checked control buffer.
                        let (clen, level, ty) = unsafe {
                            let p = (ctl + at) as *const u8;
                            (
                                (p as *const u64).read_unaligned(),
                                (p.add(8) as *const i32).read_unaligned(),
                                (p.add(12) as *const i32).read_unaligned(),
                            )
                        };
                        if clen < 16 || at + clen > ctllen {
                            return fail(EINVAL);
                        }
                        if level == 1 && ty == 1 {
                            for k in 0..(clen - 16) / 4 {
                                // SAFETY: inside this control message.
                                let fd = unsafe {
                                    ((ctl + at + 16 + 4 * k) as *const i32).read_unaligned()
                                };
                                match self.linux_file(i, fd as u64).cloned() {
                                    Some(file) if fd >= 0 => fds.push(file),
                                    _ => return fail(EBADF),
                                }
                            }
                        }
                        at += clen.div_ceil(8) * 8;
                    }
                }
                self.sock_send(i, &s, &all, fds, a3 & MSG_NOSIGNAL != 0, mem)
            }
            // recvmsg(fd, msg, flags): data into the iovecs, received
            // descriptors as an SCM_RIGHTS control message.
            47 => {
                let Some(LinuxFile::Sock(s)) = self.linux_file(i, a1).cloned() else {
                    return fail(ENOTSOCK);
                };
                if !ok(a2, 56, true) {
                    return fail(EFAULT);
                }
                // SAFETY: checked `msghdr` (56 bytes).
                let m = unsafe { core::ptr::read_unaligned(a2 as *const [u64; 7]) };
                let (name, iov, iovlen, ctl, ctllen) = (m[0], m[2], m[3], m[4], m[5]);
                if iovlen > 1024 || (iovlen > 0 && !ok(iov, iovlen * 16, false)) {
                    return fail(EINVAL);
                }
                if ctl != 0 && ctllen > 0 && !ok(ctl, ctllen, true) {
                    return fail(EFAULT);
                }
                let mut pieces = Vec::new();
                for k in 0..iovlen {
                    // SAFETY: inside the checked iovec array.
                    pieces.push(unsafe {
                        let p = (iov + 16 * k) as *const u64;
                        (p.read_unaligned(), p.add(1).read_unaligned())
                    });
                }
                let (r, got) = self.sock_recv(i, &s, &pieces, mem);
                let Outcome::Ret(n) = r else {
                    return r;
                };
                if n < 0 {
                    return r;
                }
                // Received descriptors go into the table first.
                let cloexec = a3 & MSG_CMSG_CLOEXEC != 0;
                let mut new_fds = Vec::new();
                for file in got {
                    match self.alloc_fd(i, 0, file, cloexec) {
                        Ok(fd) => new_fds.push(fd as i32),
                        Err(_) => break,
                    }
                }
                let room = if ctl == 0 {
                    0
                } else {
                    ctllen.saturating_sub(16) / 4
                };
                let mut flags = 0i32;
                let fit = (new_fds.len() as u64).min(room) as usize;
                for &fd in &new_fds[fit..] {
                    // No room in the control buffer: closed, as Linux does.
                    self.proc(i).linux_files[fd as usize] = None;
                    flags |= MSG_CTRUNC;
                }
                let used = if fit > 0 { 16 + 4 * fit as u64 } else { 0 };
                // SAFETY: `msghdr` and the control buffer were checked as
                // writable; `used` fits the control buffer.
                unsafe {
                    if fit > 0 {
                        let p = ctl as *mut u8;
                        (p as *mut u64).write_unaligned(used);
                        (p.add(8) as *mut i32).write_unaligned(1); // SOL_SOCKET
                        (p.add(12) as *mut i32).write_unaligned(1); // SCM_RIGHTS
                        for (k, fd) in new_fds[..fit].iter().enumerate() {
                            (p.add(16 + 4 * k) as *mut i32).write_unaligned(*fd);
                        }
                    }
                    let h = a2 as *mut u8;
                    (h.add(40) as *mut u64).write_unaligned(used.div_ceil(8) * 8);
                    (h.add(48) as *mut i32).write_unaligned(flags);
                    if name != 0 {
                        // An unnamed peer: no address.
                        (h.add(8) as *mut u32).write_unaligned(0);
                    }
                }
                Outcome::Ret(n)
            }
            // eventfd(initval), eventfd2(initval, flags)
            284 | 290 => {
                let flags = if number == 290 { a2 } else { 0 };
                if flags & !(EFD_SEMAPHORE | O_NONBLOCK | O_CLOEXEC) != 0 {
                    return fail(EINVAL);
                }
                let file = LinuxFile::Event(Arc::new(Mutex::new(EventFd {
                    count: a1 & 0xFFFF_FFFF,
                    semaphore: flags & EFD_SEMAPHORE != 0,
                })));
                match self.alloc_fd(i, 0, file, flags & O_CLOEXEC != 0) {
                    Ok(fd) => {
                        self.proc(i).set_nonblock(fd, flags & O_NONBLOCK != 0);
                        Outcome::Ret(fd as i64)
                    }
                    Err(e) => fail(e),
                }
            }
            // poll(fds, n, timeout ms), ppoll(fds, n, timespec, sigmask):
            // the signal mask argument is not applied.
            7 | 271 => {
                let (fds, n) = (a1, a2);
                if n > 4096 {
                    return fail(EINVAL);
                }
                if n > 0 && !ok(fds, n * 8, true) {
                    return fail(EFAULT);
                }
                let timeout = if number == 7 {
                    let ms = a3 as i32;
                    (ms >= 0).then(|| ms as u64 * 1_000_000)
                } else if a3 == 0 {
                    None
                } else {
                    match read_timespec(&ok, a3) {
                        Ok(ns) => Some(ns),
                        Err(e) => return fail(e),
                    }
                };
                let mut count = 0i64;
                for k in 0..n {
                    let at = fds + 8 * k;
                    // SAFETY: inside the checked `pollfd` array.
                    let (fd, events) = unsafe {
                        (
                            (at as *const i32).read_unaligned(),
                            ((at + 4) as *const u16).read_unaligned(),
                        )
                    };
                    let rev = if fd < 0 {
                        0
                    } else {
                        match self.linux_file(i, fd as u64) {
                            None => POLLNVAL,
                            Some(file) => {
                                Self::linux_ready(file) & (u64::from(events) | POLLERR | POLLHUP)
                            }
                        }
                    };
                    // SAFETY: inside the checked `pollfd` array.
                    unsafe { ((at + 6) as *mut u16).write_unaligned(rev as u16) };
                    if rev != 0 {
                        count += 1;
                    }
                }
                self.linux_wait_result(i, count, timeout)
            }
            // select(n, read, write, except, timeval),
            // pselect6(n, read, write, except, timespec, sigmask)
            23 | 270 => {
                let n = a1 as usize;
                if n > 1024 {
                    return fail(EINVAL);
                }
                let words = n.div_ceil(64);
                let sets = [a2, a3, f.r10];
                for &set in &sets {
                    if set != 0 && words > 0 && !ok(set, words as u64 * 8, true) {
                        return fail(EFAULT);
                    }
                }
                let tmo = f.r8;
                let timeout = if tmo == 0 {
                    None
                } else if number == 23 {
                    if !ok(tmo, 16, false) {
                        return fail(EFAULT);
                    }
                    // SAFETY: checked readable `timeval`.
                    let (sec, usec) = unsafe {
                        let p = tmo as *const u64;
                        (p.read_unaligned(), p.add(1).read_unaligned())
                    };
                    Some(
                        sec.saturating_mul(1_000_000_000)
                            .saturating_add(usec.saturating_mul(1000)),
                    )
                } else {
                    match read_timespec(&ok, tmo) {
                        Ok(ns) => Some(ns),
                        Err(e) => return fail(e),
                    }
                };
                let mut input = [[0u64; 16]; 3];
                for (row, &set) in input.iter_mut().zip(&sets) {
                    if set == 0 {
                        continue;
                    }
                    for (w, word) in row.iter_mut().enumerate().take(words) {
                        // SAFETY: inside the checked set.
                        *word = unsafe { ((set + 8 * w as u64) as *const u64).read_unaligned() };
                    }
                }
                let mut output = [[0u64; 16]; 3];
                let mut count = 0i64;
                for fd in 0..n {
                    let (w, b) = (fd / 64, 1u64 << (fd % 64));
                    if (input[0][w] | input[1][w] | input[2][w]) & b == 0 {
                        continue;
                    }
                    let Some(file) = self.linux_file(i, fd as u64) else {
                        return fail(EBADF);
                    };
                    let r = Self::linux_ready(file);
                    let want = [POLLIN | POLLHUP | POLLERR, POLLOUT | POLLERR, POLLPRI];
                    for k in 0..3 {
                        if input[k][w] & b != 0 && r & want[k] != 0 {
                            output[k][w] |= b;
                            count += 1;
                        }
                    }
                }
                let outcome = self.linux_wait_result(i, count, timeout);
                if !matches!(outcome, Outcome::Block) {
                    for (row, &set) in output.iter().zip(&sets) {
                        if set == 0 {
                            continue;
                        }
                        for (w, word) in row.iter().enumerate().take(words) {
                            // SAFETY: inside the checked set.
                            unsafe { ((set + 8 * w as u64) as *mut u64).write_unaligned(*word) };
                        }
                    }
                }
                outcome
            }
            // epoll_create(size), epoll_create1(flags)
            213 | 291 => {
                let cloexec = number == 291 && a1 & O_CLOEXEC != 0;
                if (number == 213 && a1 as i32 <= 0) || (number == 291 && a1 & !O_CLOEXEC != 0) {
                    return fail(EINVAL);
                }
                let file = LinuxFile::Epoll(Arc::new(Mutex::new(Vec::new())));
                match self.alloc_fd(i, 0, file, cloexec) {
                    Ok(fd) => Outcome::Ret(fd as i64),
                    Err(e) => fail(e),
                }
            }
            // epoll_ctl(epfd, op, fd, event)
            233 => {
                let (fd, ev) = (a3, f.r10);
                let Some(LinuxFile::Epoll(ep)) = self.linux_file(i, a1).cloned() else {
                    return fail(if self.linux_file(i, a1).is_some() {
                        EINVAL
                    } else {
                        EBADF
                    });
                };
                match self.linux_file(i, fd) {
                    None => return fail(EBADF),
                    Some(LinuxFile::Epoll(_)) => return fail(EINVAL),
                    Some(_) if fd == a1 => return fail(EINVAL),
                    Some(_) => {}
                }
                let (events, data) = if a2 == 2 {
                    (0, 0)
                } else {
                    if !ok(ev, 12, false) {
                        return fail(EFAULT);
                    }
                    // SAFETY: checked readable `epoll_event` (packed: u32, u64).
                    unsafe {
                        (
                            u64::from((ev as *const u32).read_unaligned()),
                            ((ev + 4) as *const u64).read_unaligned(),
                        )
                    }
                };
                let mut items = ep.lock();
                let at = items.iter().position(|it| it.fd == fd);
                match (a2, at) {
                    (1, Some(_)) => fail(EEXIST),
                    (1, None) => {
                        items.push(EpollItem {
                            fd,
                            events,
                            data,
                            off: false,
                        });
                        Outcome::Ret(0)
                    }
                    (2, Some(k)) => {
                        items.remove(k);
                        Outcome::Ret(0)
                    }
                    (3, Some(k)) => {
                        items[k] = EpollItem {
                            fd,
                            events,
                            data,
                            off: false,
                        };
                        Outcome::Ret(0)
                    }
                    (2 | 3, None) => fail(ENOENT),
                    _ => fail(EINVAL),
                }
            }
            // epoll_wait(epfd, events, max, timeout ms), epoll_pwait(…,
            // sigmask), epoll_pwait2(…, timespec, sigmask): level-triggered
            // (EPOLLET is treated as level), EPOLLONESHOT honoured.
            232 | 281 | 441 => {
                let (out, max) = (a2, a3 as i32);
                let Some(LinuxFile::Epoll(ep)) = self.linux_file(i, a1).cloned() else {
                    return fail(EINVAL);
                };
                if max <= 0 {
                    return fail(EINVAL);
                }
                if !ok(out, max as u64 * 12, true) {
                    return fail(EFAULT);
                }
                let timeout = if number == 441 {
                    if f.r10 == 0 {
                        None
                    } else {
                        match read_timespec(&ok, f.r10) {
                            Ok(ns) => Some(ns),
                            Err(e) => return fail(e),
                        }
                    }
                } else {
                    let ms = f.r10 as i32;
                    (ms >= 0).then(|| ms as u64 * 1_000_000)
                };
                let items: Vec<EpollItem> = ep.lock().clone();
                let (mut count, mut fired, mut gone) = (0i64, Vec::new(), Vec::new());
                for it in items.iter().filter(|it| !it.off) {
                    let Some(file) = self.linux_file(i, it.fd) else {
                        gone.push(it.fd);
                        continue;
                    };
                    let r = Self::linux_ready(file) & (it.events | POLLERR | POLLHUP) & 0xFFFF;
                    if r == 0 {
                        continue;
                    }
                    let at = out + 12 * count as u64;
                    // SAFETY: inside the checked output array (`count < max`).
                    unsafe {
                        (at as *mut u32).write_unaligned(r as u32);
                        ((at + 4) as *mut u64).write_unaligned(it.data);
                    }
                    if it.events & EPOLLONESHOT != 0 {
                        fired.push(it.fd);
                    }
                    count += 1;
                    if count == max as i64 {
                        break;
                    }
                }
                if !fired.is_empty() || !gone.is_empty() {
                    let mut items = ep.lock();
                    items.retain(|it| !gone.contains(&it.fd));
                    for it in items.iter_mut().filter(|it| fired.contains(&it.fd)) {
                        it.off = true;
                    }
                }
                self.linux_wait_result(i, count, timeout)
            }
            // sched_yield
            24 => Outcome::Yield,
            // set_robust_list: accepted (robust mutexes are not cleaned up).
            273 => Outcome::Ret(0),
            // membarrier(cmd, flags): with one CPU every thread already sees
            // memory in program order, so each barrier is complete at once.
            // QUERY reports GLOBAL, PRIVATE_EXPEDITED and its registration.
            324 => match a1 {
                0 => Outcome::Ret(1 | 8 | 16),
                1 | 8 | 16 => Outcome::Ret(0),
                _ => fail(EINVAL),
            },
            // futex(addr, op, val, timeout, addr2, val3): private futexes in
            // this process; WAIT (relative), WAIT_BITSET (absolute, all
            // bits), WAKE, WAKE_BITSET, REQUEUE, CMP_REQUEUE.
            202 => {
                let (uaddr, op, val) = (a1, a2 & 127, a3 & 0xFFFF_FFFF);
                if uaddr % 4 != 0 || !ok(uaddr, 4, false) {
                    return fail(EFAULT);
                }
                // SAFETY (reads of `uaddr` below): four readable, aligned user
                // bytes (checked above); other threads may write them.
                let current =
                    || u64::from(unsafe { core::ptr::read_volatile(uaddr as *const u32) });
                match op {
                    0 | 9 => {
                        let tmo = f.r10;
                        let first = tmo != 0 && self.task(i).sleep_until.is_none();
                        let wait_ticks = if first {
                            if !ok(tmo, 16, false) {
                                return fail(EFAULT);
                            }
                            // SAFETY: checked as readable user memory.
                            let (sec, nsec) = unsafe {
                                let p = tmo as *const u64;
                                (p.read_unaligned(), p.add(1).read_unaligned())
                            };
                            if nsec >= 1_000_000_000 {
                                return fail(EINVAL);
                            }
                            let mut ns = sec.saturating_mul(1_000_000_000).saturating_add(nsec);
                            if op == 9 {
                                let now = if a2 & 256 != 0 {
                                    rtc::realtime_ns().unwrap_or(0)
                                } else {
                                    rtc::monotonic_ns()
                                };
                                ns = ns.saturating_sub(now);
                            }
                            let tick_ns = 1_000_000_000 / u64::from(interrupts::TIMER_HZ);
                            Some(ns.div_ceil(tick_ns))
                        } else {
                            None
                        };
                        let now = interrupts::ticks();
                        let value = current();
                        let t = self.task(i);
                        if t.futex_woken {
                            t.futex_woken = false;
                            t.futex = None;
                            t.sleep_until = None;
                            return Outcome::Ret(0);
                        }
                        if value != val {
                            t.futex = None;
                            t.sleep_until = None;
                            return fail(EAGAIN);
                        }
                        if let Some(d) = wait_ticks {
                            t.sleep_until = Some(now.saturating_add(d));
                        }
                        if let Some(d) = t.sleep_until
                            && now >= d
                        {
                            t.futex = None;
                            t.sleep_until = None;
                            return fail(ETIMEDOUT);
                        }
                        t.futex = Some(uaddr);
                        Outcome::Block
                    }
                    1 | 10 => Outcome::Ret(self.linux_futex_wake(i, uaddr, val as usize, None)),
                    3 | 4 => {
                        if op == 4 && current() != f.r9 & 0xFFFF_FFFF {
                            return fail(EAGAIN);
                        }
                        let to = f.r8;
                        if !to.is_multiple_of(4) {
                            return fail(EINVAL);
                        }
                        let more = Some((to, f.r10 as usize));
                        Outcome::Ret(self.linux_futex_wake(i, uaddr, val as usize, more))
                    }
                    _ => fail(ENOSYS),
                }
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
            sys::SHM_CREATE => {
                // a1 = pages.
                if a1 == 0 || a1 > zoo_abi::SHM_MAX_PAGES {
                    return err(Error::Inval);
                }
                if self.proc(i).free_slots() < 1 || (mem.frames.available() as u64) < a1 + 64 {
                    return err(Error::NoMem);
                }
                let mut frames = Vec::with_capacity(a1 as usize);
                for _ in 0..a1 {
                    match mem.zeroed_frame() {
                        Ok(f) => frames.push(f),
                        Err(_) => {
                            for f in frames {
                                let _ = mem.frames.deallocate(f);
                            }
                            return err(Error::NoMem);
                        }
                    }
                }
                let h = self
                    .proc(i)
                    .insert(HandleEntry {
                        object: Object::Shm(Arc::new(ShmPages { frames })),
                        rights: 0,
                    })
                    .expect("checked a free slot");
                Outcome::Ret(h as i64)
            }
            sys::SHM_MAP => {
                // a1 = handle; maps every page read/write, returns the address.
                let pages = match self.proc(i).get(a1) {
                    Some(HandleEntry {
                        object: Object::Shm(p),
                        ..
                    }) => p.clone(),
                    Some(_) => return err(Error::Rights),
                    None => return err(Error::BadHandle),
                };
                let n = pages.frames.len() as u64;
                let va = self.proc(i).map_next;
                for (k, &frame) in pages.frames.iter().enumerate() {
                    if mem
                        .map_user_frame(p4, va + k as u64 * 4096, frame, true)
                        .is_err()
                    {
                        mem.unmap_user_keep(p4, va, k as u64);
                        return err(Error::NoMem);
                    }
                }
                let t = self.proc(i);
                t.shm_maps.push((va, n, pages));
                // Leave an unmapped page between mappings.
                t.map_next = va + (n + 1) * 4096;
                Outcome::Ret(va as i64)
            }
            sys::SHM_UNMAP => {
                // a1 = an address `shm_map` returned.
                let Some(k) = self.proc(i).shm_maps.iter().position(|m| m.0 == a1) else {
                    return err(Error::NotFound);
                };
                let (va, n, _pages) = self.proc(i).shm_maps.remove(k);
                mem.unmap_user_keep(p4, va, n);
                Outcome::Ret(0)
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
        // Free what ended programs held now, not at the next tick, so the
        // next program reuses warm frames (docs/f2-design.md).
        self.reap(mem);
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
    load_segments(mem, p4, file, image)?;
    let stack_bottom = USER_STACK_TOP - USER_STACK_SIZE;
    let mut page = stack_bottom;
    let mut top = None;
    while page < USER_STACK_TOP {
        top = Some(mem.map_user_page(p4, page, true, false).map_err(|_| ())?);
        page += 4096;
    }
    top.ok_or(())
}

/// The image of a program: `ET_EXEC`, or for Linux programs also
/// position-independent (`ET_DYN`), placed at `LINUX_PIE_BASE`.
fn program_image(file: &[u8], linux: bool) -> Result<elf::Image, elf::ElfError> {
    match elf::parse(file, USER_IMAGE_START, USER_IMAGE_END) {
        Err(elf::ElfError::NotExecutable) if linux => {
            elf::parse_at(file, USER_IMAGE_START, USER_IMAGE_END, Some(LINUX_PIE_BASE))
        }
        other => other,
    }
}

/// The dynamic loader a Linux program names in `PT_INTERP`, from the
/// initrd, placed at `LINUX_INTERP_BASE` (docs/linux8-design.md).
fn linux_interp(
    file: &'static [u8],
    image: &elf::Image,
) -> Result<Option<(&'static [u8], elf::Image)>, i64> {
    let Some((off, len)) = image.interp else {
        return Ok(None);
    };
    let raw = &file[off as usize..(off + len - 1) as usize];
    let path = core::str::from_utf8(raw).map_err(|_| ENOENT)?;
    let data = initrd::find(&linux_resolve("", path)).ok_or(ENOENT)?;
    let loader = elf::parse_at(
        data,
        USER_IMAGE_START,
        USER_IMAGE_END,
        Some(LINUX_INTERP_BASE),
    )
    .map_err(|_| ELIBBAD)?;
    if loader.interp.is_some() {
        return Err(ELIBBAD);
    }
    Ok(Some((data, loader)))
}

/// Maps and fills an image's segments.
fn load_segments(
    mem: &mut Memory,
    p4: PhysFrame,
    file: &[u8],
    image: &elf::Image,
) -> Result<(), ()> {
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
    Ok(())
}

// ---- Linux programs (docs/linux1-design.md) -----------------------------

/// The environment every Linux program starts with.
const LINUX_ENV: [&str; 5] = [
    "HOME=/home/user",
    "PATH=/linux",
    "USER=user",
    "LANG=C.UTF-8",
    "TERM=dumb",
];

/// Auxiliary-vector keys musl reads at start-up.
const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_ENTRY: u64 = 9;
const AT_RANDOM: u64 = 25;

/// Writes the System V process stack into the stack's top page (`page`,
/// mapped at `USER_STACK_TOP - 4096`): argument and environment strings and
/// 16 random bytes at the top, then (16-byte aligned) argc, argv, envp and
/// the auxiliary vector. Returns the initial RSP. The caller makes sure it
/// all fits the page (at most `LINUX_ARGS_MAX` of each).
fn linux_stack(
    mem: &Memory,
    page: PhysFrame,
    args: &[&str],
    vars: &[&str],
    file: &[u8],
    image: &elf::Image,
    loader: u64,
) -> u64 {
    let base = USER_STACK_TOP - 4096;
    let kva = mem.phys_to_virt(page.start_address());
    let mut top = 4096usize;
    // SAFETY (all writes below): `kva` is the freshly mapped, zeroed top
    // stack page of the new process, reached through the physical-memory
    // window; every offset stays inside it (the caller checked the sizes:
    // a spawn's command line is at most MAX_CMDLINE bytes with the fixed
    // environment, and `execve` checks its total).
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
    let mut argv = [0u64; LINUX_ARGS_MAX];
    let mut argc = 0;
    for arg in args.iter().take(argv.len()) {
        put_bytes(&mut top, &[0]);
        argv[argc] = put_bytes(&mut top, arg.as_bytes());
        argc += 1;
    }
    // The environment (docs/linux3-design.md).
    let mut env = [0u64; LINUX_ARGS_MAX];
    let mut env_count = 0;
    for v in vars.iter().take(env.len()) {
        put_bytes(&mut top, &[0]);
        env[env_count] = put_bytes(&mut top, v.as_bytes());
        env_count += 1;
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
        // Where the dynamic loader was placed (0: none).
        (AT_BASE, loader),
        (AT_RANDOM, random_at),
        (AT_NULL, 0),
    ];
    // argc, argv[], NULL, envp[], NULL, auxv pairs.
    let words = 1 + argc + 1 + env_count + 1 + 2 * aux.len();
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
    for &e in &env[..env_count] {
        word(e);
    }
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
const MAP_FIXED_NOREPLACE: u64 = 0x10_0000;
const MAP_ANONYMOUS: u64 = 0x20;
const AT_EMPTY_PATH: u64 = 0x1000;
const O_DIRECTORY: u64 = 0o200_000;
const O_CREAT: u64 = 0o100;
const O_EXCL: u64 = 0o200;
const O_TRUNC: u64 = 0o1000;
const O_APPEND: u64 = 0o2000;
const AT_REMOVEDIR: u64 = 0x200;
const O_CLOEXEC: u64 = 0o2_000_000;
const O_NONBLOCK: u64 = 0o4000;
const AF_UNIX: u64 = 1;
const SOCK_STREAM: u64 = 1;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_NOSIGNAL: u64 = 0x4000;
const MSG_CMSG_CLOEXEC: u64 = 0x4000_0000;
const MSG_CTRUNC: i32 = 8;
const EFD_SEMAPHORE: u64 = 1;
const POLLIN: u64 = 0x1;
const POLLPRI: u64 = 0x2;
const POLLOUT: u64 = 0x4;
const POLLERR: u64 = 0x8;
const POLLHUP: u64 = 0x10;
const POLLNVAL: u64 = 0x20;
const POLLRDHUP: u64 = 0x2000;
const EPOLLONESHOT: u64 = 1 << 30;
const CLONE_VM: u64 = 0x100;
const CLONE_VFORK: u64 = 0x4000;
const SIGILL: u64 = 4;
const SIGTRAP: u64 = 5;
const SIGBUS: u64 = 7;
const SIGFPE: u64 = 8;
const SIGKILL: u64 = 9;
const SIGSEGV: u64 = 11;
const SIGPIPE: u64 = 13;
const SIGSTOP: u64 = 19;
/// SIGKILL and SIGSTOP can be neither blocked nor caught.
const UNBLOCKABLE: u64 = 1 << (SIGKILL - 1) | 1 << (SIGSTOP - 1);
const SA_RESTORER: u64 = 0x0400_0000;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;
const SS_ONSTACK: u64 = 1;
const SS_DISABLE: u64 = 2;
const SS_AUTODISARM: u64 = 1 << 31;
/// A signal frame: return address (8), `ucontext` (304), `siginfo` (128).
const SIGFRAME: u64 = 440;
/// Flags a signal handler's return may set: CF PF AF ZF SF DF OF.
const LINUX_USER_FLAGS: u64 = 0xCD5;
const CLONE_FS: u64 = 0x200;
const CLONE_FILES: u64 = 0x400;
const CLONE_SIGHAND: u64 = 0x800;
const CLONE_THREAD: u64 = 0x1_0000;
const CLONE_SYSVSEM: u64 = 0x4_0000;
const CLONE_SETTLS: u64 = 0x8_0000;
const CLONE_PARENT_SETTID: u64 = 0x10_0000;
const CLONE_CHILD_CLEARTID: u64 = 0x20_0000;
const CLONE_DETACHED: u64 = 0x40_0000;
const CLONE_CHILD_SETTID: u64 = 0x100_0000;
/// A thread: `clone` must share all of these and may add the next ones.
const THREAD_NEEDS: u64 = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;
const THREAD_MAY: u64 = CLONE_SYSVSEM
    | CLONE_SETTLS
    | CLONE_PARENT_SETTID
    | CLONE_CHILD_CLEARTID
    | CLONE_DETACHED
    | CLONE_CHILD_SETTID;

/// Removes `[start, start + len)` from a list of lazy ranges, splitting
/// those it cuts.
fn lazy_cut(list: &mut Vec<(u64, u64, u64)>, start: u64, len: u64) {
    let end = start.saturating_add(len);
    let mut out = Vec::with_capacity(list.len() + 1);
    for &(s, l, p) in list.iter() {
        let e = s + l;
        if e <= start || s >= end {
            out.push((s, l, p));
            continue;
        }
        if s < start {
            out.push((s, start - s, p));
        }
        if e > end {
            out.push((end, e - end, p));
        }
    }
    *list = out;
}

/// The recorded access of the lazy range holding `va`, if any.
fn lazy_prot(list: &[(u64, u64, u64)], va: u64) -> Option<u64> {
    list.iter()
        .find(|&&(s, l, _)| va >= s && va - s < l)
        .map(|&(_, _, p)| p)
}

/// Signals whose default action is to do nothing here: SIGCHLD, SIGCONT,
/// the stop signals (no job control yet), SIGURG and SIGWINCH.
fn default_ignored(sig: u64) -> bool {
    matches!(sig, 17..=23 | 28)
}
/// Most arguments (and, separately, environment strings) at start-up, and
/// the longest single one `execve` takes.
const LINUX_ARGS_MAX: usize = 64;
const LINUX_ARG_MAX: usize = 1024;
/// Entries in the auxiliary vector `linux_stack` writes (with AT_NULL).
const LINUX_AUX_COUNT: usize = 8;
/// Where position-independent Linux programs and the dynamic loader go, in
/// the program window (docs/linux8-design.md).
const LINUX_PIE_BASE: u64 = 0x0100_0000;
const LINUX_INTERP_BASE: u64 = 0x3000_0000;

fn page_up(a: u64) -> u64 {
    a.div_ceil(4096) * 4096
}

/// A NUL-terminated user string into `buf`; its length, or `None` if it
/// is unreadable or too long.
fn user_cstr(ok: &impl Fn(u64, u64, bool) -> bool, addr: u64, buf: &mut [u8]) -> Option<usize> {
    for (k, slot) in buf.iter_mut().enumerate() {
        let a = addr.checked_add(k as u64)?;
        // One check per page: the rest of a checked page is readable too.
        if (k == 0 || a & 0xFFF == 0) && !ok(a, 1, false) {
            return None;
        }
        // SAFETY: checked above as part of a readable user page.
        let b = unsafe { (a as *const u8).read() };
        if b == 0 {
            return Some(k);
        }
        *slot = b;
    }
    None
}

/// An open Linux descriptor: an initrd file and its position, an initrd
/// folder and the next entry to list, or the same in the home folder
/// (paths relative to it, docs/linux4-design.md).
#[derive(Clone)]
enum LinuxFile {
    /// Standard input (at its end) or output (the program's log).
    Console {
        input: bool,
    },
    /// One end of a pipe (docs/linux5-design.md).
    Pipe(PipeEnd),
    /// A Unix stream socket, an eventfd, an epoll set
    /// (docs/linux7-design.md); duplicates share the object.
    Sock(Arc<Mutex<Sock>>),
    Event(Arc<Mutex<EventFd>>),
    Epoll(Arc<Mutex<Vec<EpollItem>>>),
    File(&'static [u8], u64),
    Dir(String, usize),
    Home {
        path: String,
        pos: u64,
        write: bool,
        append: bool,
    },
    HomeDir(String, usize),
}

/// A pipe: a ring of `PIPE_PAGES` physical pages (not the kernel heap),
/// with counts of the descriptors open on each end.
struct Pipe {
    frames: Vec<PhysFrame>,
    head: usize,
    len: usize,
    readers: usize,
    writers: usize,
    /// Bytes ever written and read, and descriptors sent with the byte at
    /// a given count (a Unix socket's `SCM_RIGHTS`, docs/linux7-design.md).
    total_in: u64,
    total_out: u64,
    fds: VecDeque<(u64, Vec<LinuxFile>)>,
}

impl Pipe {
    fn new(mem: &mut Memory) -> Option<Pipe> {
        let mut frames = Vec::with_capacity(PIPE_PAGES);
        for _ in 0..PIPE_PAGES {
            match mem.frames.allocate() {
                Some(f) => frames.push(f),
                None => {
                    for f in frames {
                        let _ = mem.frames.deallocate(f);
                    }
                    return None;
                }
            }
        }
        Some(Pipe {
            frames,
            head: 0,
            len: 0,
            readers: 0,
            writers: 0,
            total_in: 0,
            total_out: 0,
            fds: VecDeque::new(),
        })
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        // Freed by the next reap, which holds the memory lock.
        SHM_FREED.lock().append(&mut self.frames);
    }
}

/// One descriptor's end of a pipe; copies and drops keep the counts.
struct PipeEnd {
    pipe: Arc<Mutex<Pipe>>,
    write: bool,
}

impl PipeEnd {
    fn new(pipe: Arc<Mutex<Pipe>>, write: bool) -> PipeEnd {
        {
            let mut p = pipe.lock();
            if write {
                p.writers += 1;
            } else {
                p.readers += 1;
            }
        }
        PipeEnd { pipe, write }
    }
}

impl Clone for PipeEnd {
    fn clone(&self) -> PipeEnd {
        PipeEnd::new(self.pipe.clone(), self.write)
    }
}

impl Drop for PipeEnd {
    fn drop(&mut self) {
        let mut p = self.pipe.lock();
        if self.write {
            p.writers -= 1;
        } else {
            p.readers -= 1;
        }
    }
}

/// A Unix stream socket's state. A connection is two pipes, one each way.
enum Sock {
    Fresh,
    Bound(String),
    Listening {
        name: String,
        /// Connections not yet accepted: (incoming, outgoing, peer pid,
        /// peer name).
        backlog: VecDeque<(PipeEnd, PipeEnd, u64, String)>,
        max: usize,
        owner: u64,
    },
    Connected {
        rx: PipeEnd,
        /// `None` after `shutdown` for writing.
        tx: Option<PipeEnd>,
        peer_pid: u64,
        local: String,
        peer: String,
    },
}

/// An eventfd counter.
struct EventFd {
    count: u64,
    semaphore: bool,
}

/// The largest eventfd count.
const EVENTFD_MAX: u64 = u64::MAX - 1;

/// One descriptor in an epoll set: events wanted, user data, and whether
/// EPOLLONESHOT has disabled it.
#[derive(Clone)]
struct EpollItem {
    fd: u64,
    events: u64,
    data: u64,
    off: bool,
}

/// The name in a `sockaddr_un` at `addr` (`len` bytes): "@" and the bytes
/// of an abstract name, or "/" and a path resolved against `cwd`.
fn unix_name(
    ok: &impl Fn(u64, u64, bool) -> bool,
    addr: u64,
    len: u64,
    cwd: &str,
) -> Result<String, i64> {
    if !(3..=110).contains(&len) {
        return Err(EINVAL);
    }
    if !ok(addr, len, false) {
        return Err(EFAULT);
    }
    let mut raw = [0u8; 110];
    // SAFETY: checked as `len` readable user bytes; `len <= 110`.
    unsafe { core::ptr::copy_nonoverlapping(addr as *const u8, raw.as_mut_ptr(), len as usize) };
    if u16::from_le_bytes([raw[0], raw[1]]) != AF_UNIX as u16 {
        return Err(EAFNOSUPPORT);
    }
    let path = &raw[2..len as usize];
    if path[0] == 0 {
        let mut s = String::from("@");
        s.extend(path[1..].iter().map(|&b| char::from(b)));
        return Ok(s);
    }
    let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
    let text = core::str::from_utf8(&path[..end]).map_err(|_| EINVAL)?;
    let mut s = String::from("/");
    s.push_str(&linux_resolve(cwd, text));
    Ok(s)
}

/// Writes a `sockaddr_un` for `name` (as `unix_name` makes them; "" for an
/// unnamed socket) at `addr`, room `*lenp`, and stores its full length.
fn put_unix_name(
    ok: &impl Fn(u64, u64, bool) -> bool,
    addr: u64,
    lenp: u64,
    name: &str,
) -> Result<(), i64> {
    if !ok(lenp, 4, true) {
        return Err(EFAULT);
    }
    // SAFETY: checked as writable user memory.
    let room = unsafe { (lenp as *const u32).read_unaligned() } as usize;
    let mut raw = Vec::with_capacity(112);
    raw.extend_from_slice(&(AF_UNIX as u16).to_le_bytes());
    if let Some(abs) = name.strip_prefix('@') {
        raw.push(0);
        raw.extend(abs.chars().map(|c| c as u8));
    } else if !name.is_empty() {
        raw.extend_from_slice(name.as_bytes());
        raw.push(0);
    }
    let n = raw.len().min(room);
    if n > 0 && !ok(addr, n as u64, true) {
        return Err(EFAULT);
    }
    // SAFETY: both checked as writable user memory.
    unsafe {
        core::ptr::copy_nonoverlapping(raw.as_ptr(), addr as *mut u8, n);
        (lenp as *mut u32).write_unaligned(raw.len() as u32);
    }
    Ok(())
}

/// A `timespec` at `addr` in nanoseconds.
fn read_timespec(ok: &impl Fn(u64, u64, bool) -> bool, addr: u64) -> Result<u64, i64> {
    if !ok(addr, 16, false) {
        return Err(EFAULT);
    }
    // SAFETY: checked readable `timespec`.
    let (sec, nsec) = unsafe {
        let p = addr as *const u64;
        (p.read_unaligned(), p.add(1).read_unaligned())
    };
    if nsec >= 1_000_000_000 {
        return Err(EINVAL);
    }
    Ok(sec.saturating_mul(1_000_000_000).saturating_add(nsec))
}

/// Pipe size (as Linux's default) and the size written all at once.
const PIPE_PAGES: usize = 16;
const PIPE_SIZE: usize = PIPE_PAGES * 4096;
const PIPE_BUF: usize = 4096;

/// Copies `n` bytes between `buf` and a pipe ring from ring offset `at`
/// (wrapping): into the ring if `into`, else out of it.
fn ring_copy(mem: &Memory, frames: &[PhysFrame], at: usize, buf: *mut u8, n: usize, into: bool) {
    let mut done = 0;
    while done < n {
        let pos = (at + done) % PIPE_SIZE;
        let (k, o) = (pos / 4096, pos % 4096);
        let span = (n - done).min(4096 - o);
        // SAFETY: `k` < PIPE_PAGES and `o + span` <= 4096, so the ring side
        // is inside one of its pages (physical window); the caller checked
        // `buf..buf + n` for the direction used.
        unsafe {
            let ring = mem.phys_to_virt(frames[k].start_address()).add(o);
            if into {
                core::ptr::copy_nonoverlapping(buf.add(done), ring, span);
            } else {
                core::ptr::copy_nonoverlapping(ring, buf.add(done), span);
            }
        }
        done += span;
    }
}

/// What one BadgerHandler request gave: a reply (status, bytes), a wait
/// for it, or a Linux error before it could be sent.
enum FsStep {
    Done(i64, Vec<u8>),
    Wait,
    Fail(i64),
}

/// The reply of one request in a Linux call: blocks the call until it
/// arrives, or ends the call with an error.
macro_rules! step {
    ($e:expr) => {
        match $e {
            FsStep::Done(status, bytes) => (status, bytes),
            FsStep::Wait => return Outcome::Block,
            FsStep::Fail(e) => return Outcome::Ret(-e),
        }
    };
}
use step;

/// Most file bytes in one BadgerHandler reply.
const FS_MAX_DATA: usize = MAX_MESSAGE - 8;

/// Size from an encoded directory entry (kind u8, size u64, name).
fn entry_size(entry: &[u8]) -> u64 {
    entry
        .get(1..9)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}

/// A ZooOS error status from BadgerHandler as a Linux error number.
fn zoo_errno(status: i64) -> i64 {
    match Error::from_return(status) {
        Some(Error::NotFound) => ENOENT,
        Some(Error::Exists) => EEXIST,
        Some(Error::NotEmpty) => ENOTEMPTY,
        Some(Error::NotDir) => ENOTDIR,
        Some(Error::Inval) => EINVAL,
        Some(Error::Rights) => EACCES,
        Some(Error::NoMem) => ENOSPC,
        _ => EIO,
    }
}

/// BadgerHandler's request codes (user/src/fsproto.rs).
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Op {
    Open = 1,
    List = 2,
    Stat = 3,
    Read = 4,
    CreateDir = 5,
    RemoveDir = 6,
    CreateFile = 7,
    Write = 8,
    Truncate = 9,
    RemoveFile = 10,
    Rename = 11,
    ReadBulk = 12,
    WriteBulk = 13,
}

/// A Linux program's shared buffer with BadgerHandler: 16 pages, as
/// `zoo_rt` uses (user/src/fsproto.rs).
const LINUX_BULK_PAGES: u64 = 16;
const LINUX_BULK: usize = LINUX_BULK_PAGES as usize * 4096;
/// The handle-message tag for a shared buffer (fsproto `TAG_SHM`).
const TAG_SHM: u64 = 5;

/// Makes a shared buffer and sends it on home-folder channel `home` of
/// `proc` (a handle message, ahead of any request). `None` if either step
/// fails; the program then uses the message path.
fn attach_bulk(mem: &mut Memory, proc: &Proc, home: u64) -> Option<Arc<ShmPages>> {
    let Some(HandleEntry {
        object: Object::Endpoint { chan, side },
        ..
    }) = proc.get(home)
    else {
        return None;
    };
    let mut frames = Vec::with_capacity(LINUX_BULK_PAGES as usize);
    for _ in 0..LINUX_BULK_PAGES {
        match mem.zeroed_frame() {
            Ok(f) => frames.push(f),
            Err(_) => {
                for f in frames {
                    let _ = mem.frames.deallocate(f);
                }
                return None;
            }
        }
    }
    let pages = Arc::new(ShmPages { frames });
    let mut c = chan.lock();
    let peer = 1 - *side;
    if !c.open[peer] || c.queues[peer].len() >= CHANNEL_DEPTH {
        return None; // dropping `pages` queues its frames to be freed
    }
    let entry = HandleEntry {
        object: Object::Shm(pages.clone()),
        rights: 0,
    };
    c.queues[peer].push_back(Msg::Handle(entry, TAG_SHM));
    Some(pages)
}

/// Copies `data` into the start of the shared buffer, page by page.
fn bytes_to_shm(mem: &Memory, shm: &ShmPages, data: &[u8]) {
    for (k, piece) in data.chunks(4096).enumerate() {
        let to = mem.phys_to_virt(shm.frames[k].start_address());
        // SAFETY: `to` is frame `k` of the buffer through the physical-memory
        // window; BadgerHandler does not use the buffer until the request
        // that follows; `piece` is at most one page.
        unsafe { core::ptr::copy_nonoverlapping(piece.as_ptr(), to, piece.len()) };
    }
}

/// Copies the first `n` bytes of the shared buffer to user address `to`
/// (checked writable by the caller), page by page.
fn shm_to_user(mem: &Memory, shm: &ShmPages, to: u64, n: usize) {
    let mut done = 0;
    for frame in &shm.frames {
        if done == n {
            break;
        }
        let k = (n - done).min(4096);
        let from = mem.phys_to_virt(frame.start_address());
        // SAFETY: BadgerHandler answered, so it is done with the buffer;
        // `to + done .. + k` is inside the checked user range.
        unsafe { core::ptr::copy_nonoverlapping(from, (to + done as u64) as *mut u8, k) };
        done += k;
    }
}

/// What an initrd path names.
enum Kind {
    File(&'static [u8]),
    Dir,
}

/// The uid and gid Linux programs see.
const LINUX_UID: u32 = 1000;

/// `path` against working directory `cwd` (both without a leading `/`),
/// with `.` and `..` applied: `/etc/services` → `etc/services`.
fn linux_resolve(cwd: &str, path: &str) -> String {
    let mut parts: alloc::vec::Vec<&str> = if path.starts_with('/') {
        alloc::vec::Vec::new()
    } else {
        cwd.split('/').filter(|c| !c.is_empty()).collect()
    };
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    parts.join("/")
}

/// A file, or a folder: the root, or any prefix of a packed file's path.
fn initrd_kind(path: &str) -> Option<Kind> {
    if path.is_empty() {
        return Some(Kind::Dir);
    }
    if let Some(data) = initrd::find(path) {
        return Some(Kind::File(data));
    }
    let mut dir = false;
    initrd::for_each(|name, _| {
        dir |= name.len() > path.len()
            && name.starts_with(path)
            && name.as_bytes()[path.len()] == b'/';
    });
    dir.then_some(Kind::Dir)
}

/// The names directly inside folder `dir`, sorted, with whether each is a
/// folder.
fn initrd_children(dir: &str) -> alloc::vec::Vec<(String, bool)> {
    let mut found = alloc::collections::BTreeMap::new();
    initrd::for_each(|name, _| {
        let rest = if dir.is_empty() {
            Some(name)
        } else {
            name.strip_prefix(dir).and_then(|r| r.strip_prefix('/'))
        };
        if let Some(rest) = rest {
            let (first, more) = match rest.split_once('/') {
                Some((first, _)) => (first, true),
                None => (rest, false),
            };
            if !first.is_empty() {
                *found.entry(String::from(first)).or_insert(false) |= more;
            }
        }
    });
    found.into_iter().collect()
}

/// Linux error numbers used here.
const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const ESRCH: i64 = 3;
const EINTR: i64 = 4;
const ENODEV: i64 = 19;
const ELIBBAD: i64 = 80;
const ETIMEDOUT: i64 = 110;
const ENOTSOCK: i64 = 88;
const ENOPROTOOPT: i64 = 92;
const ESOCKTNOSUPPORT: i64 = 94;
const EAFNOSUPPORT: i64 = 97;
const EADDRINUSE: i64 = 98;
const EISCONN: i64 = 106;
const ENOTCONN: i64 = 107;
const ECONNREFUSED: i64 = 111;
const E2BIG: i64 = 7;
const ENOEXEC: i64 = 8;
const ECHILD: i64 = 10;
const ESPIPE: i64 = 29;
const EPIPE: i64 = 32;
const ENOTDIR: i64 = 20;
const EIO: i64 = 5;
const EAGAIN: i64 = 11;
const EACCES: i64 = 13;
const EEXIST: i64 = 17;
const EXDEV: i64 = 18;
const ENOSPC: i64 = 28;
const ENAMETOOLONG: i64 = 36;
const ENOTEMPTY: i64 = 39;
const EISDIR: i64 = 21;
const ERANGE: i64 = 34;
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
        // Preemption: round-robin. The kernel task takes a turn only when it
        // has work; idle, it would halt until the next tick while a program
        // is ready (docs/f1-design.md: the 10 ms stalls).
        s.prefer_user = KERNEL_IDLE.load(Ordering::Relaxed);
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
                if s.linux_deliver(cur, f, m) {
                    frame
                } else {
                    s.switch_away(frame, m)
                }
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
            Outcome::Resume => {
                if s.linux_deliver(cur, f, m) {
                    frame
                } else {
                    s.switch_away(frame, m)
                }
            }
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
        if s.linux_fault(cur, f, addr, m) || s.deliver_exception(cur, f, addr, m) {
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
