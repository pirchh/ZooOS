//! Child-process helpers with bounded waits and guaranteed cleanup.

use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

/// Kills and reaps the child when dropped, so no exit path leaks a process.
pub struct ChildGuard {
    child: Option<Child>,
    name: String,
}

/// Puts `child` in a job object that ends it when xtask ends, however xtask
/// ends (Ctrl+C, a crash, being killed): `Drop` below does not run then.
/// `run --mic` once left QEMU listening to the microphone after Ctrl+C
/// (docs/listening-design.md §7). The job's handle is never closed here;
/// Windows closes it as xtask exits, and that ends every process in it.
#[cfg(windows)]
fn end_with_xtask(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;

    #[repr(C)]
    struct BasicLimits {
        per_process_user_time: i64,
        per_job_user_time: i64,
        limit_flags: u32,
        minimum_working_set: usize,
        maximum_working_set: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }
    #[repr(C)]
    struct IoCounters([u64; 6]);
    /// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
    #[repr(C)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *mut std::ffi::c_void, name: *const u16) -> isize;
        fn SetInformationJobObject(
            job: isize,
            class: i32,
            info: *const std::ffi::c_void,
            len: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: isize, process: isize) -> i32;
    }

    static JOB: OnceLock<Option<isize>> = OnceLock::new();
    let job = *JOB.get_or_init(|| {
        // SAFETY: no attributes and no name (null pointers are allowed);
        // the result is a handle or 0.
        let job = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        if job == 0 {
            return None;
        }
        // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION is valid
        // (no limits); only the kill-on-close flag is set.
        let mut limits: ExtendedLimits = unsafe { std::mem::zeroed() };
        limits.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is a live JOBOBJECT_EXTENDED_LIMIT_INFORMATION of
        // the size passed, for the job handle just created.
        let ok = unsafe {
            SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&raw const limits).cast(),
                std::mem::size_of::<ExtendedLimits>() as u32,
            )
        };
        (ok != 0).then_some(job)
    });
    let assigned = job.is_some_and(|job| {
        // SAFETY: `job` is the job created above, kept open for the rest of
        // the process; the child's handle is live while `child` is.
        unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as isize) != 0 }
    });
    if !assigned {
        eprintln!(
            "[xtask] warning: could not tie process {} to xtask; if xtask is killed, end it by hand",
            child.id()
        );
    }
}

#[cfg(not(windows))]
fn end_with_xtask(_child: &Child) {}

impl ChildGuard {
    pub fn new(child: Child, name: &str) -> Self {
        end_with_xtask(&child);
        ChildGuard {
            child: Some(child),
            name: name.to_string(),
        }
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, String> {
        let child = self.child.as_mut().expect("child present until shutdown");
        child
            .try_wait()
            .map_err(|e| format!("failed to poll {}: {e}", self.name))
    }

    pub fn wait(&mut self) -> Result<ExitStatus, String> {
        let child = self.child.as_mut().expect("child present until shutdown");
        let status = child
            .wait()
            .map_err(|e| format!("failed to wait for {}: {e}", self.name))?;
        self.child = None;
        Ok(status)
    }

    /// Terminates the child (if still running) and waits up to `bound` for it
    /// to be reaped. Returns how it ended, or an error if it would not die.
    pub fn shutdown(&mut self, bound: Duration) -> Result<Shutdown, String> {
        let Some(child) = self.child.as_mut() else {
            return Ok(Shutdown::AlreadyReaped);
        };
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            self.child = None;
            return Ok(Shutdown::Exited(status));
        }
        // An error here means it exited between the poll and the kill; the
        // wait loop below reaps it either way.
        let _ = child.kill();
        let deadline = Instant::now() + bound;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.child = None;
                    return Ok(Shutdown::Killed);
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Ok(None) => {
                    return Err(format!(
                        "{} (pid {}) did not exit within {:?} after kill",
                        self.name,
                        child.id(),
                        bound
                    ));
                }
                Err(e) => return Err(format!("failed to reap {}: {e}", self.name)),
            }
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.child.is_some()
            && let Err(e) = self.shutdown(Duration::from_secs(5))
        {
            eprintln!("warning: {e}");
        }
    }
}

#[derive(Debug)]
pub enum Shutdown {
    /// Exited on its own before we stopped it.
    Exited(ExitStatus),
    /// Was running and was terminated by the harness.
    Killed,
    AlreadyReaped,
}

/// Runs a short command and captures its output, killing it after `timeout`.
pub fn output_bounded(mut cmd: Command, timeout: Duration, name: &str) -> Result<Output, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to start {name}: {e}"))?;
    let pid = child.id();
    // Read pipes on a helper thread so a full pipe cannot block the child.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(r) => r.map_err(|e| format!("failed to run {name}: {e}")),
        Err(_) => {
            kill_pid(pid);
            Err(format!(
                "{name} did not finish within {timeout:?} and was terminated"
            ))
        }
    }
}

fn kill_pid(pid: u32) {
    let (prog, args): (&str, Vec<String>) = if cfg!(windows) {
        (
            "taskkill",
            vec!["/F".into(), "/PID".into(), pid.to_string()],
        )
    } else {
        ("kill", vec!["-9".into(), pid.to_string()])
    };
    let _ = Command::new(prog)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
