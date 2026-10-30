//! Child-process helpers with bounded waits and guaranteed cleanup.

use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

/// Kills and reaps the child when dropped, so no exit path leaks a process.
pub struct ChildGuard {
    child: Option<Child>,
    name: String,
}

impl ChildGuard {
    pub fn new(child: Child, name: &str) -> Self {
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
