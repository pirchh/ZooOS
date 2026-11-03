//! QEMU invocation: shared machine definition, interactive run, and the
//! bounded headless runner used by `verify`.

use crate::config::Tools;
use crate::image::Mode;
use crate::markers::{self, Expectation, Verdict};
use crate::process::{ChildGuard, Shutdown};
use crate::qmp::{self, Qmp};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(50);
const KILL_BOUND: Duration = Duration::from_secs(10);

static ACCEL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Selects the QEMU accelerator for this run (`tcg` unless set). Hardware
/// acceleration is optional and never required (DECISIONS.md).
pub fn set_accel(name: &str) {
    let _ = ACCEL.set(name.to_string());
}

/// USB devices on the test machine (docs/h1-design.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usb {
    /// No USB controller.
    None,
    /// A keyboard and a mouse on the controller's own ports.
    Direct,
    /// A hub on a root port, with the keyboard and mouse behind it.
    Hub,
    /// A USB tablet (absolute pointer, no mouse grab); keyboard on PS/2.
    /// What `run` uses (docs/u1c-design.md).
    Tablet,
}

static USB: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The display device of the next machine.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// QEMU's standard VGA: a plain framebuffer, as on real hardware's
    /// firmware framebuffer.
    Vga,
    /// `virtio-vga`: VGA for the firmware and console, virtio-gpu (flushed
    /// updates, hardware cursor) for the desktop (docs/gpu1-design.md).
    VirtioVga,
}

static VIRTIO_DISPLAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A screen size passed to a virtio GPU guest through fw_cfg
/// (`opt/zooos/display`, docs/u2b-design.md): width << 32 | height, 0 for
/// none.
static DISPLAY_SIZE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The screen size the next virtio GPU machine asks for, if any.
pub fn set_display_size(size: Option<(u32, u32)>) {
    let v = size.map_or(0, |(w, h)| u64::from(w) << 32 | u64::from(h));
    DISPLAY_SIZE.store(v, std::sync::atomic::Ordering::Relaxed);
}

/// The host's primary monitor in physical pixels (Windows only).
#[cfg(windows)]
pub fn host_screen() -> Option<(u32, u32)> {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn SetProcessDPIAware() -> i32;
        fn GetSystemMetrics(index: i32) -> i32;
    }
    // SAFETY: plain Win32 calls without pointers. Declaring this process
    // DPI-aware makes the metrics physical pixels instead of scaled ones.
    let (w, h) = unsafe {
        SetProcessDPIAware();
        (GetSystemMetrics(0), GetSystemMetrics(1))
    };
    (w > 0 && h > 0).then_some((w as u32, h as u32))
}

#[cfg(not(windows))]
pub fn host_screen() -> Option<(u32, u32)> {
    None
}

/// The display device the next machine gets.
pub fn set_display(d: Display) {
    VIRTIO_DISPLAY.store(
        d == Display::VirtioVga,
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The network the next machine gets (docs/n1-design.md).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Net {
    /// No network device.
    None = 0,
    /// QEMU's user network, cut off from the host and the internet
    /// (`restrict=on`): its DHCP server and gateway answer, nothing else.
    Isolated = 1,
    /// QEMU's user network with outgoing connections (for `run`).
    Open = 2,
}

static NET: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
/// A host port QEMU forwards the guest's 10.0.2.100:80 to (0: none;
/// docs/n2-design.md).
static GUESTFWD: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

/// Forwards the guest's TCP connections to 10.0.2.100:80 to host port
/// `port` on 127.0.0.1 (even on the isolated network).
pub fn set_guestfwd(port: Option<u16>) {
    GUESTFWD.store(port.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
}

/// The read-only model disk the next machine gets, if any
/// (docs/stage2-design.md §3).
static MODEL_DISK: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

pub fn set_model_disk(disk: Option<PathBuf>) {
    *MODEL_DISK.lock().unwrap_or_else(|e| e.into_inner()) = disk;
}

/// The guest's memory in GiB (2 unless changed; docs/talk-design.md).
static MEMORY_GIB: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

pub fn set_memory_gib(gib: u8) {
    MEMORY_GIB.store(gib, std::sync::atomic::Ordering::Relaxed);
}

pub fn set_net(net: Net) {
    NET.store(net as u8, std::sync::atomic::Ordering::Relaxed);
}

/// The USB devices the next machine gets.
pub fn set_usb(usb: Usb) {
    USB.store(usb as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn accel() -> String {
    ACCEL.get().cloned().unwrap_or_else(|| "tcg".into())
}

/// QEMU splits option values on `,`; a literal comma is written as `,,`.
fn opt_path(p: &Path) -> Result<String, String> {
    let s = p
        .to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", p.display()))?;
    Ok(s.replace(',', ",,"))
}

/// The machine definition shared by `run` and `verify` (see DECISIONS.md).
/// Software emulation (TCG) only, so no host hypervisor is required.
/// `headless` hides the display window; the emulated screen still exists
/// (verify takes screenshots of it).
/// The data disk created for a run: 1 GiB (docs/linux10c-design.md). The
/// image file is sparse, so the host stores only the blocks written.
pub const DATA_DISK_BYTES: u64 = 1 << 30;

/// Creates (or, with `reset`, recreates) a zero-filled data disk at `path`,
/// as a sparse file. NTFS reserves the whole length of a file extended
/// otherwise: 1 GiB per case, which filled tens of GB of the host's disk.
pub fn ensure_data_disk(path: &Path, reset: bool) -> Result<(), String> {
    if reset || !path.is_file() {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        let f =
            fs::File::create(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        if cfg!(windows) {
            let out = std::process::Command::new("fsutil")
                .args(["sparse", "setflag"])
                .arg(path)
                .output()
                .map_err(|e| format!("cannot run fsutil: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "fsutil sparse setflag {} failed: {}",
                    path.display(),
                    String::from_utf8_lossy(&out.stdout).trim()
                ));
            }
        }
        f.set_len(DATA_DISK_BYTES)
            .map_err(|e| format!("cannot size {}: {e}", path.display()))?;
    }
    Ok(())
}

fn machine_args(
    tools: &Tools,
    image: &Path,
    vars: &Path,
    headless: bool,
    data: &Path,
) -> Result<Vec<OsString>, String> {
    let accel = accel();
    let code = tools
        .ovmf_code
        .as_deref()
        .expect("resolved by resolve_for_qemu");
    let mut args = vec![
        "-machine".to_string(),
        "q35".into(),
        "-accel".into(),
        accel.clone(),
        "-smp".into(),
        "1".into(),
        "-m".into(),
        format!("{}G", MEMORY_GIB.load(std::sync::atomic::Ordering::Relaxed)),
        "-drive".into(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            opt_path(code)?
        ),
        "-drive".into(),
        format!("if=pflash,format=raw,unit=1,file={}", opt_path(vars)?),
        "-drive".into(),
        format!("format=raw,media=disk,file={}", opt_path(image)?),
        // The data disk: legacy virtio (I/O-port interface), which is
        // what the kernel's driver speaks.
        "-drive".into(),
        format!("if=none,id=data,format=raw,file={}", opt_path(data)?),
        "-device".into(),
        "virtio-blk-pci,drive=data,disable-modern=on".into(),
        "-monitor".into(),
        "none".into(),
        "-no-reboot".into(),
    ];
    // The model disk: read-only, so a guest bug cannot change it.
    if let Some(disk) = MODEL_DISK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        args.extend([
            "-drive".into(),
            format!(
                "if=none,id=model,format=raw,readonly=on,file={}",
                opt_path(disk)?
            ),
            "-device".into(),
            "virtio-blk-pci,drive=model,disable-modern=on".into(),
        ]);
    }
    // TCG uses the fixed `qemu64` model. With a hardware accelerator QEMU's
    // default model is used: `-cpu max` made OVMF fault under WHPX.
    if accel == "tcg" {
        args.extend(["-cpu".into(), "qemu64".into()]);
    }
    if headless {
        args.extend(["-display".into(), "none".into()]);
    }
    // A display whose preferred mode (in its EDID) is 1920x1080, the mode
    // the firmware then starts in (docs/u1-design.md).
    let virtio = VIRTIO_DISPLAY.load(std::sync::atomic::Ordering::Relaxed);
    let size = DISPLAY_SIZE.load(std::sync::atomic::Ordering::Relaxed);
    let device = if virtio && size != 0 {
        let (w, h) = (size >> 32, size & 0xFFFF_FFFF);
        args.extend([
            "-fw_cfg".into(),
            format!("name=opt/zooos/display,string={w}x{h}"),
        ]);
        format!("virtio-vga,xres={w},yres={h},edid=on")
    } else if virtio {
        "virtio-vga,xres=1920,yres=1080,edid=on".into()
    } else {
        "VGA,xres=1920,yres=1080,edid=on".into()
    };
    args.extend(["-vga".into(), "none".into(), "-device".into(), device]);
    // The network: legacy virtio (the kernel's driver), no boot ROM.
    let net = NET.load(std::sync::atomic::Ordering::Relaxed);
    if net == Net::None as u8 {
        args.extend(["-nic".into(), "none".into()]);
    } else {
        let restrict = if net == Net::Isolated as u8 {
            ",restrict=on"
        } else {
            ""
        };
        let fwd = match GUESTFWD.load(std::sync::atomic::Ordering::Relaxed) {
            0 => String::new(),
            port => format!(",guestfwd=tcp:10.0.2.100:80-tcp:127.0.0.1:{port}"),
        };
        args.extend([
            "-netdev".into(),
            format!("user,id=net0{restrict}{fwd}"),
            "-device".into(),
            "virtio-net-pci,netdev=net0,disable-modern=on,romfile=".into(),
        ]);
    }
    let usb = USB.load(std::sync::atomic::Ordering::Relaxed);
    if usb == Usb::Direct as u8 {
        args.extend([
            "-device".into(),
            "qemu-xhci,id=xhci".into(),
            "-device".into(),
            "usb-kbd,bus=xhci.0".into(),
            "-device".into(),
            "usb-mouse,bus=xhci.0".into(),
        ]);
    } else if usb == Usb::Tablet as u8 {
        args.extend([
            "-device".into(),
            "qemu-xhci,id=xhci".into(),
            "-device".into(),
            "usb-tablet,bus=xhci.0".into(),
        ]);
    } else if usb == Usb::Hub as u8 {
        args.extend([
            "-device".into(),
            "qemu-xhci,id=xhci".into(),
            "-device".into(),
            "usb-hub,bus=xhci.0,port=1".into(),
            "-device".into(),
            "usb-kbd,bus=xhci.0,port=1.1".into(),
            "-device".into(),
            "usb-mouse,bus=xhci.0,port=1.2".into(),
        ]);
    }
    Ok(args.into_iter().map(OsString::from).collect())
}

/// Fresh per-run directory holding a private copy of the variable store, so
/// the shared template is never written. Only generated files live here.
fn prepare_run_dir(root: &Path, case: &str, tools: &Tools) -> Result<(PathBuf, PathBuf), String> {
    let dir = root.join("build").join("runs").join(case);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("cannot clear {}: {e}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let template = tools
        .ovmf_vars_template
        .as_deref()
        .expect("resolved by resolve_for_qemu");
    let vars = dir.join("OVMF_VARS.fd");
    fs::copy(template, &vars).map_err(|e| {
        format!(
            "cannot copy variable store {} -> {}: {e}",
            template.display(),
            vars.display()
        )
    })?;
    Ok((dir, vars))
}

fn write_cmdline(dir: &Path, qemu: &Path, args: &[OsString]) {
    let mut s = format!("\"{}\"", qemu.display());
    for a in args {
        s.push_str(&format!(" \"{}\"", a.to_string_lossy()));
    }
    s.push('\n');
    let _ = fs::write(dir.join("qemu-command.txt"), s);
}

pub fn run_interactive(
    root: &Path,
    tools: &Tools,
    mode: Mode,
    image: &Path,
    timeout: Option<Duration>,
) -> Result<ExitCode, String> {
    let qemu = tools.qemu.as_deref().expect("resolved by resolve_for_qemu");
    let (dir, vars) = prepare_run_dir(root, &format!("run-{}", mode.name()), tools)?;
    // Kept between runs, so files saved in one session are there next time.
    let data = root.join("build").join("data.img");
    ensure_data_disk(&data, false)?;
    println!(
        "[run] data disk {} (delete it to start empty)",
        data.display()
    );
    // A tablet: the pointer follows the host's exactly, without a grab.
    set_usb(Usb::Tablet);
    // A network (docs/n1-design.md).
    set_net(Net::Open);
    // A virtio GPU: updates shown when flushed, and a hardware cursor that
    // the host draws (docs/gpu1-design.md).
    set_display(Display::VirtioVga);
    // A monitor larger than 1920x1080: a screen of its size, so full
    // screen (Ctrl+Alt+F) is one guest pixel per monitor pixel. From
    // 3200x1800 up it is drawn at 2x (docs/u2b-design.md). QEMU scales its
    // window to fit otherwise.
    let host = host_screen().filter(|&(w, h)| w > 1920 || h > 1080);
    set_display_size(host);
    if let Some((w, h)) = host {
        let scale = if w >= 3200 && h >= 1800 { 2 } else { 1 };
        println!(
            "[run] host monitor {w}x{h}: a {w}x{h} screen at {scale}x (one-to-one in full screen, Ctrl+Alt+F)"
        );
    }
    let mut args = machine_args(tools, image, &vars, false, &data)?;
    if host.is_some() {
        args.extend(["-display", "gtk,zoom-to-fit=on"].map(OsString::from));
    }
    args.extend(["-serial", "stdio"].map(OsString::from));
    write_cmdline(&dir, qemu, &args);
    println!(
        "[run] booting {} image in a QEMU window (accelerator {}); type there. Guest COM1 follows here.",
        mode.name(),
        accel()
    );
    println!("[run] Close the window or press Ctrl+C here to stop QEMU.");
    let child = Command::new(qemu)
        .args(&args)
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", qemu.display()))?;
    let mut guard = ChildGuard::new(child, "QEMU");
    let Some(limit) = timeout else {
        let status = guard.wait()?;
        println!("\n[run] QEMU exited with {status}");
        return Ok(ExitCode::SUCCESS);
    };
    let start = Instant::now();
    loop {
        if let Some(status) = guard.try_wait()? {
            println!("\n[run] QEMU exited with {status}");
            return Ok(ExitCode::SUCCESS);
        }
        if start.elapsed() >= limit {
            guard.shutdown(KILL_BOUND)?;
            println!("\n[run] stopped QEMU after {limit:?}");
            return Ok(ExitCode::SUCCESS);
        }
        std::thread::sleep(POLL);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    /// A wrong, out-of-order or extra marker appeared.
    UnexpectedMarker(String),
    /// The deadline passed while still waiting for a marker.
    Timeout(String),
    /// QEMU exited by itself (crash, triple fault with -no-reboot, bad args).
    EarlyExit(String),
    /// The harness itself failed (QMP connection, screenshot), so the guest
    /// was not properly tested. Never counts as a pass.
    HarnessError(String),
}

impl Outcome {
    pub fn kind(&self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::UnexpectedMarker(_) => "unexpected-marker",
            Outcome::Timeout(_) => "timeout",
            Outcome::EarlyExit(_) => "early-exit",
            Outcome::HarnessError(_) => "harness-error",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Outcome::Pass => "all markers observed in order",
            Outcome::UnexpectedMarker(s)
            | Outcome::Timeout(s)
            | Outcome::EarlyExit(s)
            | Outcome::HarnessError(s) => s,
        }
    }
}

pub struct CaseRun {
    pub outcome: Outcome,
    pub elapsed: Duration,
    pub log: PathBuf,
    /// How QEMU was stopped; a halted guest is expected to be `Killed`.
    pub shutdown: String,
    /// Screenshots taken by `Action::Screenshot`, as (name, PPM path).
    pub screenshots: Vec<(String, PathBuf)>,
}

/// One step of driving the guest through QMP (docs/m4-design.md §8).
pub enum Action {
    /// Wait until this complete serial line appears after the previous
    /// `WaitFor` match.
    WaitFor(String),
    /// Like `WaitFor`, but the line only has to contain this text (for
    /// lines with timestamps).
    WaitContains(String),
    /// Fails the case if any line so far contains this text: for something
    /// that must not have happened (put a `Settle` before it).
    NeverSeen(String),
    /// Press and release these QEMU qcodes, one at a time.
    Keys(Vec<&'static str>),
    /// Wait until the serial log has not grown for this long (the guest has
    /// finished reacting), then a little more for the screen to be drawn.
    Settle(Duration),
    /// Save `<name>.ppm` (checked by the case) and `<name>.png` (for people).
    Screenshot(&'static str),
    /// Move the mouse by (dx, dy) screen pixels, in steps of at most
    /// `MOUSE_STEP` (y grows downwards).
    Mouse(i32, i32),
    /// Move a tablet's pointer to this absolute position (0..=0x7FFF on
    /// each axis).
    MouseAbs(u32, u32),
    /// Tablet positions sent back to back, with no pause: a fast hand.
    MouseAbsBurst(Vec<(u32, u32)>),
    /// Press (true) or release the left mouse button.
    Button(bool),
    /// Press (true) or release the right mouse button.
    RightButton(bool),
    /// Hold this key (a QEMU qcode) down for this many milliseconds.
    HoldKey(&'static str, u64),
    /// Press (true) or release this key (a QEMU qcode), and leave it so.
    Key(&'static str, bool),
    /// Wait for a line containing this text followed by Unix seconds, and
    /// require them to be within `CLOCK_TOLERANCE` of the host's clock.
    WaitClock(String),
    /// Wait until some line anywhere in the log contains this text, without
    /// moving past it: for lines whose order against others is not fixed
    /// (a service's start-up next to `SHELL ready`).
    WaitSeen(String),
    /// Wait for a line containing this text, and require it to arrive
    /// between `min` and `max` milliseconds (host time) after the previous
    /// waited-for line.
    WaitGap(String, u64, u64),
    /// Wait for a line containing the text, and require the number after
    /// each (marker, min, max) in it to be within min..=max.
    WaitNumbers(String, Vec<(&'static str, u64, u64)>),
}

/// Steps to perform, in order, while the case runs. All of them must finish
/// before the case can pass.
pub struct Interaction {
    pub steps: Vec<Action>,
}

const KEY_GAP: Duration = Duration::from_millis(40);
/// How far the guest's wall clock may be from the host's.
const CLOCK_TOLERANCE: u64 = 120;
/// Largest single mouse movement sent; PS/2 packets carry at most 255.
const MOUSE_STEP: i32 = 100;
const QMP_CONNECT_BOUND: Duration = Duration::from_secs(10);

/// Time limits for one case.
pub struct Limits {
    /// Deadline for all markers to appear.
    pub timeout: Duration,
    /// How long to keep watching after the last marker for trailing ones.
    pub settle: Duration,
}

/// Boots `image` headlessly with COM1 written by QEMU itself to a log file,
/// and evaluates that log until pass, failure, early exit or the timeout.
/// After a pass, keeps watching for the settle time to catch trailing markers
/// (e.g. READY after PANIC). QEMU is always terminated and reaped.
#[allow(clippy::too_many_arguments)]
pub fn run_case(
    root: &Path,
    tools: &Tools,
    case: &str,
    image: &Path,
    exp: &Expectation,
    limits: Limits,
    interaction: Option<&Interaction>,
    data: &Path,
) -> Result<CaseRun, String> {
    let Limits { timeout, settle } = limits;
    let qemu = tools.qemu.as_deref().expect("resolved by resolve_for_qemu");
    let (dir, vars) = prepare_run_dir(root, case, tools)?;
    let log = dir.join("serial.log");
    // ZOO_VERIFY_WINDOW=1 shows QEMU's window during verify cases, to
    // measure what its display costs (docs/u2-compositor-design.md).
    let headless = std::env::var_os("ZOO_VERIFY_WINDOW").is_none();
    let mut args = machine_args(tools, image, &vars, headless, data)?;
    args.extend(
        [
            "-chardev".to_string(),
            format!("file,id=com1,path={}", opt_path(&log)?),
            "-serial".into(),
            "chardev:com1".into(),
        ]
        .map(OsString::from),
    );
    let qmp_port = match interaction {
        Some(_) => {
            let port = qmp::free_port()?;
            args.push("-qmp".into());
            args.push(format!("tcp:127.0.0.1:{port},server=on,wait=off").into());
            Some(port)
        }
        None => None,
    };
    write_cmdline(&dir, qemu, &args);
    let open = |name: &str| {
        fs::File::create(dir.join(name)).map_err(|e| format!("cannot create {name}: {e}"))
    };
    let child = Command::new(qemu)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(open("qemu-stdout.log")?)
        .stderr(open("qemu-stderr.log")?)
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", qemu.display()))?;
    let mut guard = ChildGuard::new(child, "QEMU");

    let start = Instant::now();
    let mut qmp_conn: Option<Qmp> = None;
    if let Some(port) = qmp_port {
        match Qmp::connect(port, QMP_CONNECT_BOUND) {
            Ok(q) => qmp_conn = Some(q),
            Err(e) => {
                guard.shutdown(KILL_BOUND)?;
                return Ok(CaseRun {
                    outcome: Outcome::HarnessError(e),
                    elapsed: start.elapsed(),
                    log,
                    shutdown: "terminated by harness".into(),
                    screenshots: Vec::new(),
                });
            }
        }
    }
    let mut step = 0usize;
    // Byte offset in the log just past the last `WaitFor` match.
    let mut cursor = 0usize;
    let mut last_len = 0usize;
    let mut last_growth = Instant::now();
    let mut step_started = Instant::now();
    // When the last waited-for line was seen (for `WaitGap`).
    let mut last_match = Instant::now();
    let mut screenshots = Vec::new();
    let steps: &[Action] = interaction.map_or(&[], |i| &i.steps);
    let mut passed_at: Option<Instant> = None;
    // When each serial line was first seen (milliseconds since QEMU
    // started, to the poll interval), written to `serial-times.log`.
    let mut times = String::new();
    let mut timed_upto = 0usize;
    let outcome = loop {
        let text = fs::read(&log)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        if text.len() != last_len {
            last_len = text.len();
            last_growth = Instant::now();
            let complete = text.rfind('\n').map_or(0, |i| i + 1);
            if complete > timed_upto {
                let ms = start.elapsed().as_millis();
                for line in text[timed_upto..complete].lines() {
                    let _ = writeln!(times, "{ms:>7} {line}");
                }
                timed_upto = complete;
            }
        }
        // Run as many interaction steps as are ready now.
        let mut harness_error = None;
        while let (Some(action), Some(q)) = (steps.get(step), qmp_conn.as_mut()) {
            let done = match action {
                Action::WaitFor(line) => {
                    // Only complete lines count, as for markers.
                    let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
                    match find_line(complete, cursor, line) {
                        Some(end) => {
                            cursor = end;
                            last_match = Instant::now();
                            true
                        }
                        None => false,
                    }
                }
                Action::WaitContains(part) => {
                    let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
                    match find_line_containing(complete, cursor, part) {
                        Some(end) => {
                            cursor = end;
                            last_match = Instant::now();
                            true
                        }
                        None => false,
                    }
                }
                Action::WaitSeen(part) => {
                    let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
                    find_line_containing(complete, 0, part).is_some()
                }
                Action::NeverSeen(part) => {
                    if find_line_containing(&text, 0, part).is_some() {
                        harness_error = Some(format!("`{part}` appeared, and must not"));
                        break;
                    }
                    true
                }
                Action::WaitGap(part, min, max) => {
                    let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
                    match find_line_containing(complete, cursor, part) {
                        Some(end) => {
                            cursor = end;
                            let gap = last_match.elapsed().as_millis() as u64;
                            last_match = Instant::now();
                            if gap < *min || gap > *max {
                                harness_error = Some(format!(
                                    "`{part}` came {gap} ms after the previous line (expected {min}-{max} ms)"
                                ));
                                break;
                            }
                            println!("[verify] `{part}` came {gap} ms after the previous line");
                            true
                        }
                        None => false,
                    }
                }
                Action::WaitNumbers(part, bounds) => {
                    let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
                    match find_line_containing(complete, cursor, part) {
                        Some(end) => {
                            let line = complete[cursor..end]
                                .lines()
                                .rev()
                                .find(|l| l.contains(part.as_str()))
                                .unwrap_or("");
                            cursor = end;
                            if let Err(e) = check_numbers(line, bounds) {
                                harness_error = Some(e);
                                break;
                            }
                            println!("[verify] in range: {}", line.trim());
                            true
                        }
                        None => false,
                    }
                }
                Action::Keys(keys) => match type_keys(q, keys) {
                    Ok(()) => true,
                    Err(e) => {
                        harness_error = Some(e);
                        break;
                    }
                },
                Action::WaitClock(part) => {
                    let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
                    match find_line_containing(complete, cursor, part) {
                        Some(end) => {
                            let line = complete[cursor..end]
                                .lines()
                                .rev()
                                .find(|l| l.contains(part.as_str()))
                                .unwrap_or("");
                            cursor = end;
                            if let Err(e) = check_clock(line, part) {
                                harness_error = Some(e);
                                break;
                            }
                            true
                        }
                        None => false,
                    }
                }
                Action::Mouse(dx, dy) => match move_mouse(q, *dx, *dy) {
                    Ok(()) => true,
                    Err(e) => {
                        harness_error = Some(e);
                        break;
                    }
                },
                Action::Key(key, down) => match q.key_event(key, *down) {
                    Ok(()) => {
                        std::thread::sleep(KEY_GAP);
                        true
                    }
                    Err(e) => {
                        harness_error = Some(e);
                        break;
                    }
                },
                Action::HoldKey(key, ms) => {
                    let held = q.key_event(key, true).and_then(|()| {
                        std::thread::sleep(Duration::from_millis(*ms));
                        q.key_event(key, false)
                    });
                    match held {
                        Ok(()) => true,
                        Err(e) => {
                            harness_error = Some(e);
                            break;
                        }
                    }
                }
                Action::MouseAbsBurst(points) => {
                    let started = Instant::now();
                    match points.iter().try_for_each(|&(x, y)| q.mouse_abs(x, y)) {
                        Ok(()) => {
                            // QEMU's main loop answers these commands; a
                            // busy display makes them slower to deliver.
                            println!(
                                "[verify] burst of {} moves delivered in {} ms",
                                points.len(),
                                started.elapsed().as_millis()
                            );
                            true
                        }
                        Err(e) => {
                            harness_error = Some(e);
                            break;
                        }
                    }
                }
                Action::MouseAbs(x, y) => match q.mouse_abs(*x, *y) {
                    Ok(()) => {
                        std::thread::sleep(KEY_GAP);
                        true
                    }
                    Err(e) => {
                        harness_error = Some(e);
                        break;
                    }
                },
                Action::RightButton(down) => match q.button("right", *down) {
                    Ok(()) => {
                        std::thread::sleep(KEY_GAP);
                        true
                    }
                    Err(e) => {
                        harness_error = Some(e);
                        break;
                    }
                },
                Action::Button(down) => match q.mouse_button(*down) {
                    Ok(()) => {
                        std::thread::sleep(KEY_GAP);
                        true
                    }
                    Err(e) => {
                        harness_error = Some(e);
                        break;
                    }
                },
                Action::Settle(quiet) => {
                    last_growth.elapsed() >= *quiet && step_started.elapsed() >= *quiet
                }
                Action::Screenshot(name) => match screenshot(q, &dir, name) {
                    Ok(ppm) => {
                        screenshots.push((name.to_string(), ppm));
                        true
                    }
                    Err(e) => {
                        harness_error = Some(e);
                        break;
                    }
                },
            };
            if !done {
                break;
            }
            step += 1;
            step_started = Instant::now();
        }
        if let Some(e) = harness_error {
            break Outcome::HarnessError(e);
        }
        let verdict = markers::evaluate(exp, &text);
        match verdict {
            Verdict::Unexpected(why) => break Outcome::UnexpectedMarker(why),
            // Markers alone are not enough while steps remain.
            Verdict::Pass if step < steps.len() => {}
            Verdict::Pass => {
                let t = *passed_at.get_or_insert_with(Instant::now);
                if t.elapsed() >= settle {
                    break Outcome::Pass;
                }
            }
            Verdict::Pending(_) => {}
        }
        if let Some(status) = guard.try_wait()? {
            // Re-read once: QEMU may have flushed final output as it exited.
            let text = fs::read(&log)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            break match markers::evaluate(exp, &text) {
                Verdict::Unexpected(why) => Outcome::UnexpectedMarker(why),
                Verdict::Pending(next) => Outcome::EarlyExit(format!(
                    "QEMU exited with {status} while waiting for `{next}`"
                )),
                Verdict::Pass => Outcome::EarlyExit(format!(
                    "QEMU exited with {status} after the markers; a halted guest should keep running"
                )),
            };
        }
        if passed_at.is_none() && start.elapsed() >= timeout {
            let next = match verdict {
                Verdict::Pending(n) => format!("`{n}`"),
                _ => match steps.get(step) {
                    Some(Action::WaitFor(l)) => format!("serial line `{l}` (step {})", step + 1),
                    Some(Action::WaitContains(l)) => {
                        format!("a serial line containing `{l}` (step {})", step + 1)
                    }
                    Some(_) => format!("interaction step {} to finish", step + 1),
                    None => "?".into(),
                },
            };
            break Outcome::Timeout(format!("no {next} within {timeout:?}"));
        }
        std::thread::sleep(POLL);
    };
    let elapsed = start.elapsed();
    let _ = fs::write(dir.join("serial-times.log"), &times);
    drop(qmp_conn);
    let shutdown = match guard.shutdown(KILL_BOUND)? {
        Shutdown::Killed => "terminated by harness".to_string(),
        Shutdown::Exited(s) => format!("exited on its own ({s})"),
        Shutdown::AlreadyReaped => "already reaped".to_string(),
    };
    Ok(CaseRun {
        outcome,
        elapsed,
        log,
        shutdown,
        screenshots,
    })
}

/// The number right after each marker in `line` must be within its bounds.
fn check_numbers(line: &str, bounds: &[(&str, u64, u64)]) -> Result<(), String> {
    for &(marker, min, max) in bounds {
        let at = line
            .find(marker)
            .ok_or_else(|| format!("no `{marker}` in `{}`", line.trim()))?;
        let digits: String = line[at + marker.len()..]
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let v: u64 = digits
            .parse()
            .map_err(|_| format!("no number after `{marker}` in `{}`", line.trim()))?;
        if v < min || v > max {
            return Err(format!(
                "`{marker}` {v} outside {min}..={max} in `{}`",
                line.trim()
            ));
        }
    }
    Ok(())
}

/// Finds a complete line equal to `want` starting at or after byte `from`;
/// returns the offset just past it.
/// Reads the Unix seconds after `part` in `line` and compares them with
/// the host clock.
fn check_clock(line: &str, part: &str) -> Result<(), String> {
    let after = &line[line.find(part).map_or(line.len(), |i| i + part.len())..];
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    let guest: u64 = digits
        .parse()
        .map_err(|_| format!("no Unix seconds after `{part}` in `{}`", line.trim()))?;
    let host = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("host clock before 1970: {e}"))?
        .as_secs();
    let diff = guest.abs_diff(host);
    if diff > CLOCK_TOLERANCE {
        return Err(format!(
            "guest clock {guest} is {diff} s from the host's {host} (allowed {CLOCK_TOLERANCE} s)"
        ));
    }
    println!("[verify] guest clock {guest}, host {host}: {diff} s apart");
    Ok(())
}

fn find_line_containing(text: &str, from: usize, part: &str) -> Option<usize> {
    let mut pos = from.min(text.len());
    for line in text[pos..].split_inclusive('\n') {
        pos += line.len();
        if line.contains(part) {
            return Some(pos);
        }
    }
    None
}

fn find_line(text: &str, from: usize, want: &str) -> Option<usize> {
    let mut pos = from.min(text.len());
    for line in text[pos..].split_inclusive('\n') {
        pos += line.len();
        // Exact, or at the end of the line: the shell prompt (`zoo> `) is
        // printed without a newline, so a kernel line that arrives while it
        // is showing shares its line, as the marker checker already allows.
        let mut line = line.trim_end();
        while let Some(rest) = line.strip_prefix("zoo> ") {
            line = rest;
        }
        if line == want {
            return Some(pos);
        }
    }
    None
}

/// Saves `<name>.ppm` (for checks) and `<name>.png` (for people).
fn screenshot(q: &mut Qmp, dir: &Path, name: &str) -> Result<PathBuf, String> {
    let ppm = dir.join(format!("{name}.ppm"));
    q.screendump(&ppm, "ppm")?;
    if !ppm.is_file() {
        return Err(format!(
            "screendump reported success but wrote no {name}.ppm"
        ));
    }
    if let Err(e) = q.screendump(&dir.join(format!("{name}.png")), "png") {
        eprintln!("[verify] note: PNG screenshot {name} failed: {e}");
    }
    Ok(ppm)
}

fn move_mouse(q: &mut Qmp, mut dx: i32, mut dy: i32) -> Result<(), String> {
    while dx != 0 || dy != 0 {
        let sx = dx.clamp(-MOUSE_STEP, MOUSE_STEP);
        let sy = dy.clamp(-MOUSE_STEP, MOUSE_STEP);
        q.mouse_move(sx, sy)?;
        dx -= sx;
        dy -= sy;
        std::thread::sleep(KEY_GAP);
    }
    Ok(())
}

fn type_keys(q: &mut Qmp, keys: &[&str]) -> Result<(), String> {
    for k in keys {
        q.send_key(k)?;
        std::thread::sleep(KEY_GAP);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::find_line;

    #[test]
    fn find_line_matches_exact_and_after_a_prompt() {
        let log = "a\nzoo> ZOO_OS PROC start pid=24 name=seal-test\nb\n";
        assert!(find_line(log, 0, "ZOO_OS PROC start pid=24 name=seal-test").is_some());
        assert!(find_line(log, 0, "a").is_some());
        // Only a prompt may come before the wanted text.
        assert!(find_line("xyz ZOO_OS READY\n", 0, "ZOO_OS READY").is_none());
        assert!(find_line(log, 0, "PROC start pid=24 name=seal-test").is_none());
    }
}
