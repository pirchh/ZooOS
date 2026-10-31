//! Minimal QMP (QEMU Machine Protocol) client over TCP on 127.0.0.1, used by
//! verify to type keys into the guest and take screenshots. Only the three
//! commands needed are implemented; responses are matched as text.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

pub struct Qmp {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

/// Asks the OS for a free localhost port. QEMU binds it moments later; the
/// small race is acceptable for a local test harness.
pub fn free_port() -> Result<u16, String> {
    let l = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot reserve a port: {e}"))?;
    let port = l.local_addr().map_err(|e| e.to_string())?.port();
    Ok(port)
}

impl Qmp {
    /// Connects (retrying until `bound`), reads the greeting and enables
    /// command mode.
    pub fn connect(port: u16, bound: Duration) -> Result<Self, String> {
        let deadline = Instant::now() + bound;
        let stream = loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => break s,
                Err(e) if Instant::now() >= deadline => {
                    return Err(format!(
                        "QMP connect to port {port} failed within {bound:?}: {e}"
                    ));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|e| e.to_string())?;
        let writer = stream.try_clone().map_err(|e| e.to_string())?;
        let mut qmp = Qmp {
            reader: BufReader::new(stream),
            writer,
        };
        let greeting = qmp.read_line()?;
        if !greeting.contains("\"QMP\"") {
            return Err(format!("unexpected QMP greeting: {greeting}"));
        }
        qmp.command(r#"{"execute":"qmp_capabilities"}"#)?;
        Ok(qmp)
    }

    fn read_line(&mut self) -> Result<String, String> {
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .map_err(|e| format!("QMP read failed: {e}"))?;
        if n == 0 {
            return Err("QMP connection closed".into());
        }
        Ok(line)
    }

    /// Sends one command and waits for its `return` (skipping async events).
    fn command(&mut self, json: &str) -> Result<String, String> {
        self.writer
            .write_all(format!("{json}\n").as_bytes())
            .map_err(|e| format!("QMP write failed: {e}"))?;
        loop {
            let line = self.read_line()?;
            if line.contains("\"return\"") {
                return Ok(line);
            }
            if line.contains("\"error\"") {
                return Err(format!("QMP error for {json}: {}", line.trim()));
            }
            // Anything else is an asynchronous event; keep reading.
        }
    }

    /// Presses and releases one key, named by its QEMU `qcode`.
    /// Presses one key, or several together when joined by `+`
    /// (`shift+p`).
    pub fn send_key(&mut self, qcode: &str) -> Result<(), String> {
        let keys: Vec<String> = qcode
            .split('+')
            .map(|k| format!(r#"{{"type":"qcode","data":"{k}"}}"#))
            .collect();
        let cmd = format!(
            r#"{{"execute":"send-key","arguments":{{"keys":[{}]}}}}"#,
            keys.join(",")
        );
        self.command(&cmd).map(|_| ())
    }

    /// Moves the mouse by (dx, dy) screen pixels (y grows downwards).
    pub fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        let cmd = format!(
            r#"{{"execute":"input-send-event","arguments":{{"events":[{{"type":"rel","data":{{"axis":"x","value":{dx}}}}},{{"type":"rel","data":{{"axis":"y","value":{dy}}}}}]}}}}"#
        );
        self.command(&cmd).map(|_| ())
    }

    /// Presses (`down`) or releases the left mouse button.
    pub fn mouse_button(&mut self, down: bool) -> Result<(), String> {
        let cmd = format!(
            r#"{{"execute":"input-send-event","arguments":{{"events":[{{"type":"btn","data":{{"down":{down},"button":"left"}}}}]}}}}"#
        );
        self.command(&cmd).map(|_| ())
    }

    /// Writes the current display to `path` (`ppm` or `png`).
    pub fn screendump(&mut self, path: &Path, format: &str) -> Result<(), String> {
        let p = path
            .to_str()
            .ok_or("screenshot path is not UTF-8")?
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let cmd = format!(
            r#"{{"execute":"screendump","arguments":{{"filename":"{p}","format":"{format}"}}}}"#
        );
        self.command(&cmd).map(|_| ())
    }
}
