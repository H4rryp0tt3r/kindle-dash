/*
 * dash-status -- the single panel owner: a current-boot streaming console.
 * Watch logs/status with inotify, block while idle, and compose one conservative
 * frame at a time, with occasional black/white clearing passes. Output arriving
 * during refresh or a short batching window is coalesced, not queued. No crates.
 */
use std::ffi::{c_char, c_int};
#[cfg(test)]
use std::fs::OpenOptions;
use std::fs::{self, File};
#[cfg(test)]
use std::io::Write;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LOG: &str = "/var/log/dash.log";
const RUN: &str = "/var/run";
const LIMIT: usize = 65536;
const WIDTH: usize = 43; // renderer allows 47; leave two-space margins
const LOG_ROWS: usize = 20;
const WATCHDOG: Duration = Duration::from_secs(20);
const COALESCE: Duration = Duration::from_millis(200);
const CLEAN_AFTER: u32 = 8;
const MASK: u32 = 0x0000_0002 | 0x0000_0008 | 0x0000_0080 | 0x0000_0100; // modify/close-write/move-to/create

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}
extern "C" {
    fn inotify_init1(flags: c_int) -> c_int;
    fn inotify_add_watch(fd: c_int, path: *const c_char, mask: u32) -> c_int;
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
}

#[derive(Clone, Debug, PartialEq)]
struct Status {
    state: String,
    detail: String,
}
impl Status {
    fn pending() -> Self {
        Self {
            state: "starting".into(),
            detail: "waiting for service".into(),
        }
    }
    fn terminal(&self) -> bool {
        self.state != "starting"
    }
    fn line(&self, name: &str, expired: bool) -> String {
        let label = if expired && !self.terminal() {
            "TIMED OUT"
        } else {
            match self.state.as_str() {
                "ready" => {
                    if name == "USB" {
                        "configured"
                    } else {
                        "listening"
                    }
                }
                "disabled" => "disabled",
                "failed" => "FAILED",
                "stopped" => "stopped",
                _ => "starting",
            }
        };
        format!("{name}: {label} - {}", self.detail)
    }
}
fn printable(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if (32..=126).contains(&b) {
                b as char
            } else {
                ' '
            }
        })
        .collect()
}
fn parse_status(text: &str) -> Status {
    if text.len() > 256 {
        return Status::pending();
    }
    let mut lines = text.lines();
    let state = lines.next().unwrap_or("");
    let detail = lines.next().unwrap_or("");
    if !["starting", "ready", "disabled", "failed", "stopped"].contains(&state)
        || lines.next().is_some()
    {
        return Status::pending();
    }
    Status {
        state: state.into(),
        detail: printable(detail),
    }
}
fn read_status(path: &Path) -> Status {
    let mut data = String::new();
    match File::open(path).and_then(|f| f.take(257).read_to_string(&mut data)) {
        Ok(_) => parse_status(&data),
        Err(_) => Status::pending(),
    }
}
fn wrap(text: &str) -> Vec<String> {
    let text = printable(text);
    if text.is_empty() {
        return vec![String::new()];
    }
    text.as_bytes()
        .chunks(WIDTH)
        .map(|b| String::from_utf8(b.to_vec()).unwrap())
        .collect()
}
fn boot_lines(data: &str) -> Vec<String> {
    let lines: Vec<_> = data.lines().collect();
    let begin = lines
        .iter()
        .rposition(|l| l.contains("stage1: start, mounts up"))
        .unwrap_or(0);
    lines[begin..]
        .iter()
        // Renderer diagnostics must not trigger their own repaint/log loop.
        .filter(|l| !l.contains("10-dash:") && !l.contains("dash-status:"))
        .flat_map(|l| wrap(l))
        .collect()
}
fn log_tail(path: &Path) -> io::Result<Vec<String>> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let offset = size.saturating_sub(LIMIT as u64);
    file.seek(SeekFrom::Start(offset))?;
    let mut data = Vec::new();
    file.take(LIMIT as u64).read_to_end(&mut data)?;
    let data = String::from_utf8_lossy(&data);
    let data = if offset > 0 {
        data.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    } else {
        &data
    };
    Ok(boot_lines(data))
}
fn frame(
    release: &str,
    kernel: &str,
    uptime: &str,
    usb: &Status,
    ssh: &Status,
    expired: bool,
    logs: &[String],
) -> String {
    let mut out = vec![
        "Hello World!".to_string(),
        format!("Dash OS {}", printable(release.trim())),
        format!("uptime {uptime}s"),
        printable(kernel.trim()),
        String::new(),
    ];
    out.extend(wrap(&usb.line("USB", expired)));
    out.extend(wrap(&ssh.line("SSH", expired)));
    out.push("Kindle: 192.168.15.244".into());
    out.push("Mac: 192.168.15.201/24".into());
    out.push("Log available in diagnostics".into());
    out.push(String::new());
    let rows = LOG_ROWS.min(40usize.saturating_sub(out.len()));
    let start = logs.len().saturating_sub(rows);
    out.extend_from_slice(&logs[start..]);
    out.into_iter()
        .take(40)
        .map(|l| {
            format!(
                "  {}\n",
                printable(&l).chars().take(WIDTH).collect::<String>()
            )
        })
        .collect()
}
fn wait(fd: c_int, timeout: c_int) -> io::Result<()> {
    let mut p = PollFd {
        fd,
        events: 1,
        revents: 0,
    };
    if unsafe { poll(&mut p, 1, timeout) } < 0 {
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
    Ok(())
}
struct Watcher(File);
impl Watcher {
    fn new(log_dir: &Path, run: &Path) -> io::Result<Self> {
        let fd = unsafe { inotify_init1(0o2000000 | 0o4000) }; // CLOEXEC/nonblock
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        for path in [log_dir, run] {
            let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid watch path"))?;
            if unsafe { inotify_add_watch(fd, path.as_ptr(), MASK) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self(file))
    }
    fn drain(&mut self) -> io::Result<()> {
        let mut buf = [0; 8192];
        loop {
            match self.0.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }
}
fn listen_inode(tcp: &str) -> Vec<String> {
    tcp.lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() > 9 && fields[1] == "F40FA8C0:0016" && fields[3] == "0A" {
                Some(fields[9].to_string())
            } else {
                None
            }
        })
        .collect()
}
fn owns_listener(proc: &Path, pid: u32) -> bool {
    let tcp = match fs::read_to_string(proc.join("net/tcp")) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let inodes = listen_inode(&tcp);
    let fds = match fs::read_dir(proc.join(pid.to_string()).join("fd")) {
        Ok(f) => f,
        Err(_) => return false,
    };
    fds.filter_map(Result::ok).any(|entry| {
        fs::read_link(entry.path()).ok().is_some_and(|p| {
            inodes
                .iter()
                .any(|i| p.as_os_str() == format!("socket:[{i}]").as_str())
        })
    })
}
fn listener(pid: u32) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if owns_listener(Path::new("/proc"), pid) {
            return Ok(());
        }
        if !Path::new("/proc").join(pid.to_string()).exists() || Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Dropbear did not own the USB listening socket",
            ));
        }
        // poll with no descriptor is a bounded readiness wait, not a boot sleep.
        wait(-1, 25)?;
    }
}
// A dirty burst gets one fixed deadline; later events cannot postpone it.
// The initial frame is immediate, and a quiet panel has no repaint deadline.
#[derive(Default)]
struct Refresh {
    displayed: bool,
    since_clean: u32,
    due: Option<Instant>,
}
impl Refresh {
    fn observe(&mut self, changed: bool, now: Instant) {
        if !changed {
            self.due = None;
        } else if self.due.is_none() {
            self.due = Some(if self.displayed { now + COALESCE } else { now });
        }
    }
    fn ready(&self, now: Instant) -> bool {
        self.due.is_some_and(|due| now >= due)
    }
    fn clean(&self) -> bool {
        !self.displayed || self.since_clean >= CLEAN_AFTER
    }
    fn displayed(&mut self, clean: bool) {
        self.displayed = true;
        self.since_clean = if clean { 0 } else { self.since_clean + 1 };
        self.due = None;
    }
}
fn poll_timeout(now: Instant, deadlines: &[Option<Instant>]) -> c_int {
    deadlines.iter().flatten().map(|due| {
        let duration = due.saturating_duration_since(now);
        // Round up so the final fraction of a millisecond cannot busy-poll.
        duration.as_millis().saturating_add(u128::from(duration.subsec_nanos() % 1_000_000 != 0))
            .min(c_int::MAX as u128) as c_int
    }).min().unwrap_or(-1)
}
fn paint(screen: &Path, frame: &Path, clean: bool) -> io::Result<()> {
    let mut command = Command::new(screen);
    if clean {
        command.arg("--clean");
    }
    let mut child = command.arg(frame).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
    // Forward before waiting: diagnostics must reach the boot log even if an
    // ioctl hangs. The prefix excludes them from the displayed log tail.
    let diagnostics = (|| {
        for line in BufReader::new(child.stderr.take().unwrap()).lines() {
            eprintln!("10-dash: screen: {}", printable(&line?));
        }
        Ok::<_, io::Error>(())
    })();
    let status = child.wait()?;
    diagnostics?;
    if !status.success() {
        return Err(io::Error::other(format!("screen failed: {status}")));
    }
    Ok(())
}
fn monitor(
    log: &Path,
    run: &Path,
    screen: &Path,
    release: &str,
    kernel: &str,
    watchdog: Duration,
) -> io::Result<()> {
    let mut watch = Watcher::new(log.parent().unwrap(), run)?;
    let deadline = Instant::now() + watchdog;
    let mut previous = String::new();
    let mut last_logs = Vec::new();
    let mut usb = Status::pending();
    let mut ssh = Status::pending();
    let mut last_expired = false;
    let mut first = true;
    let mut refresh = Refresh::default();
    loop {
        watch.drain()?;
        let logs = log_tail(log).unwrap_or_default();
        let u = read_status(&run.join("dash-usbnet.status"));
        let s = read_status(&run.join("dash-sshd.status"));
        let expired = Instant::now() >= deadline;
        let show_timeout = expired && (!u.terminal() || !s.terminal());
        // Compare with the last displayed state, not intermediate observations,
        // so a burst cannot lose its final state. Clock changes alone stay idle.
        let changed = first || logs != last_logs || u != usb || s != ssh
            || show_timeout != last_expired;
        let now = Instant::now();
        refresh.observe(changed, now);
        if refresh.ready(now) {
            let uptime = fs::read_to_string("/proc/uptime").unwrap_or_default();
            let uptime = uptime.split('.').next().unwrap_or("?");
            let next = frame(release, kernel, uptime, &u, &s, expired, &logs);
            if next != previous {
                let clean = refresh.clean();
                let path = run.join("dash.frame");
                fs::write(&path, &next)?;
                paint(screen, &path, clean)?;
                previous = next;
                refresh.displayed(clean);
            } else {
                refresh.due = None;
            }
            first = false;
            last_logs = logs;
            usb = u;
            ssh = s;
            last_expired = show_timeout;
        }
        let now = Instant::now();
        let timeout = poll_timeout(now, &[
            (!expired).then_some(deadline), refresh.due,
        ]);
        wait(watch.0.as_raw_fd(), timeout)?;
    }
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let result = if args.len() == 3 && args[1] == "listening" {
        args[2]
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid server PID"))
            .and_then(listener)
    } else if args.len() == 1 {
        let release = fs::read_to_string("/etc/dash-release").unwrap_or_else(|_| "unknown".into());
        let version = fs::read_to_string("/proc/version").unwrap_or_default();
        let kernel = version.split_whitespace().nth(2).unwrap_or("unknown");
        monitor(
            Path::new(LOG),
            Path::new(RUN),
            Path::new("/bin/screen"),
            &release,
            kernel,
            WATCHDOG,
        )
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: dash-status [listening PID]",
        ))
    };
    if let Err(error) = result {
        if args.len() == 1 {
            eprintln!("10-dash: status unavailable: {error}; panel service parked");
            // Do not let a missing renderer/watch path cause a restart/flash loop.
            // poll blocks indefinitely and normal runit signals still stop us.
            loop {
                let _ = wait(-1, -1);
            }
        }
        eprintln!("30-sshd: listener check failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    #[test]
    fn refresh_is_immediate_once_then_batches_with_a_fixed_deadline() {
        let now = Instant::now();
        let mut refresh = Refresh::default();
        refresh.observe(true, now);
        assert!(refresh.ready(now));
        assert!(refresh.clean());
        refresh.displayed(true);
        assert!(!refresh.ready(now));
        refresh.observe(true, now);
        let due = refresh.due;
        for ms in 1..200 {
            refresh.observe(true, now + Duration::from_millis(ms));
            assert_eq!(refresh.due, due);
            assert!(!refresh.ready(now + Duration::from_millis(ms)));
        }
        assert!(refresh.ready(now + COALESCE));
        refresh.observe(false, now + COALESCE);
        assert_eq!(refresh.due, None);
        assert_eq!(poll_timeout(now, &[None]), -1);
    }
    #[test]
    fn clean_after_eight_successful_ordinary_frames_not_observations() {
        let mut refresh = Refresh::default();
        refresh.displayed(true);
        for _ in 0..CLEAN_AFTER {
            assert!(!refresh.clean());
            refresh.observe(true, Instant::now());
            refresh.observe(false, Instant::now());
            refresh.displayed(false);
        }
        assert!(refresh.clean());
        // Merely deciding to clean does not advance/reset the count.
        assert!(refresh.clean());
        refresh.displayed(true);
        assert!(!refresh.clean());
        assert_eq!(refresh.since_clean, 0);
    }
    #[test]
    fn poll_uses_the_earliest_deadline_and_rounds_up() {
        let now = Instant::now();
        assert_eq!(poll_timeout(now, &[None, Some(now + COALESCE)]), 200);
        assert_eq!(poll_timeout(now, &[Some(now + COALESCE), Some(now + Duration::from_micros(500))]), 1);
        assert_eq!(poll_timeout(now, &[Some(now)]), 0);
    }
    #[test]
    fn paint_propagates_renderer_failure() {
        assert!(paint(Path::new("/bin/false"), Path::new("/unused"), true).is_err());
    }
    #[test]
    fn status_is_bounded_and_explicit() {
        assert_eq!(parse_status("ready\nUSB address\n").state, "ready");
        for text in ["bad\nready", "ready\na\nextra", &"x".repeat(257)] {
            assert_eq!(parse_status(text), Status::pending());
        }
    }
    #[test]
    fn wraps_printable_visible_rows() {
        let rows = wrap(&format!("{}\x1b\t", "a".repeat(100)));
        assert!(rows
            .iter()
            .all(|r| r.len() <= WIDTH && r.bytes().all(|b| (32..=126).contains(&b))));
        assert_eq!(rows.len(), 3);
    }
    #[test]
    fn only_current_boot_and_no_renderer_feedback() {
        let logs = boot_lines(
            "old boot\n0s stage1: start, mounts up\n1s USB failed\n10-dash: screen failed\n",
        );
        assert_eq!(logs.len(), 2);
        assert!(!logs.join("\n").contains("old boot"));
    }
    #[test]
    fn pending_times_out_but_terminal_recovers() {
        assert!(Status::pending().line("USB", true).contains("TIMED OUT"));
        assert!(!parse_status("ready\nUSB address")
            .line("USB", true)
            .contains("TIMED OUT"));
        assert!(parse_status("disabled\nnot provisioned")
            .line("SSH", true)
            .contains("disabled"));
    }
    #[test]
    fn frame_is_bounded_and_keeps_recent_output() {
        let logs: Vec<_> = (0..100).map(|i| format!("log {i}")).collect();
        let f = frame(
            "0.2.0",
            "3.0.35-lab126",
            "3",
            &Status::pending(),
            &Status::pending(),
            false,
            &logs,
        );
        assert!(f.contains("Hello World!"));
        assert!(f.contains("log 99"));
        assert!(!f.contains("log 0\n"));
        assert!(f.lines().count() <= 40);
        assert!(f.lines().all(|l| l.len() <= 45));
    }
    #[test]
    fn listener_requires_exact_usb_address_port_state() {
        let data = "0: F40FA8C0:0016 00000000:0000 0A 0 0 0 0 0 123\n1: 00000000:0016 0 0A 0 0 0 0 0 456\n2: F40FA8C0:0016 0 01 0 0 0 0 0 789";
        assert_eq!(listen_inode(data), vec!["123"]);
    }
    #[test]
    fn socket_must_belong_to_server() {
        let p = std::env::temp_dir().join(format!("dash-listener-test-{}", std::process::id()));
        fs::create_dir(&p).unwrap();
        fs::create_dir_all(p.join("net")).unwrap();
        fs::create_dir_all(p.join("42/fd")).unwrap();
        fs::write(
            p.join("net/tcp"),
            "0: F40FA8C0:0016 00000000:0000 0A 0 0 0 0 0 123\n",
        )
        .unwrap();
        assert!(!owns_listener(&p, 42));
        symlink("socket:[123]", p.join("42/fd/3")).unwrap();
        assert!(owns_listener(&p, 42));
        assert!(!owns_listener(&p, 43));
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn monitor_streams_then_is_idle_without_self_loop() {
        let p = std::env::temp_dir().join(format!("dash-monitor-test-{}", std::process::id()));
        fs::create_dir(&p).unwrap();
        fs::create_dir_all(p.join("log")).unwrap();
        fs::create_dir_all(p.join("run")).unwrap();
        let log = p.join("log/dash.log");
        fs::write(&log, "0s stage1: start, mounts up\nUSB first\n").unwrap();
        let screen = p.join("screen");
        // Stop the monitor by replacing the executable with a missing path.
        // The clock and writes are test-only, never device.
        fs::write(&screen, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}/args'\n[ \"$1\" != --clean ] || shift\ncp \"$1\" '{}/latest'\nprintf '10-dash: renderer output\\n' >> '{}'\nprintf 'frame\\n' >> '{}/frames'\n",p.display(),p.display(),log.display(),p.display())).unwrap();
        fs::set_permissions(&screen, fs::Permissions::from_mode(0o755)).unwrap();
        let log2 = log.clone();
        let run = p.join("run");
        let screen2 = screen.clone();
        let handle = std::thread::spawn(move || {
            monitor(
                &log2,
                &run,
                &screen2,
                "test",
                "kernel",
                Duration::from_millis(700),
            )
        });
        let wait_for = |count: usize| {
            let limit = Instant::now() + Duration::from_secs(3);
            loop {
                if fs::read_to_string(p.join("frames"))
                    .unwrap_or_default()
                    .lines()
                    .count()
                    >= count
                {
                    break;
                }
                assert!(Instant::now() < limit);
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        wait_for(1);
        fs::write(
            p.join("run/dash-usbnet.status"),
            "failed\nloading arcotg_udc\n",
        )
        .unwrap();
        wait_for(2);
        wait_for(3); // watchdog: SSH still pending
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(
            fs::read_to_string(p.join("frames"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        assert!(fs::read_to_string(p.join("latest"))
            .unwrap()
            .contains("TIMED OUT"));
        fs::write(
            p.join("run/dash-sshd.status"),
            "ready\nkey authentication only\n",
        )
        .unwrap();
        wait_for(4);
        assert!(!fs::read_to_string(p.join("latest"))
            .unwrap()
            .contains("TIMED OUT"));
        OpenOptions::new()
            .append(true)
            .open(&log)
            .unwrap()
            .write_all(b"new streamed line\n")
            .unwrap();
        wait_for(5);
        assert!(fs::read_to_string(p.join("latest"))
            .unwrap()
            .contains("new streamed line"));
        // Several writes within a dirty window produce just the latest frame.
        for i in 0..5 {
            fs::write(p.join("run/dash-sshd.status"), format!("ready\nburst {i}\n")).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        wait_for(6);
        std::thread::sleep(COALESCE + Duration::from_millis(50));
        assert_eq!(fs::read_to_string(p.join("frames")).unwrap().lines().count(), 6);
        assert!(fs::read_to_string(p.join("latest")).unwrap().contains("burst 4"));
        for count in 7..=10 {
            OpenOptions::new().append(true).open(&log).unwrap()
                .write_all(format!("cadence {count}\n").as_bytes()).unwrap();
            wait_for(count);
        }
        let args = fs::read_to_string(p.join("args")).unwrap();
        let clean: Vec<_> = args.lines().enumerate().filter_map(|(i, line)|
            line.starts_with("--clean ").then_some(i + 1)).collect();
        assert_eq!(clean, vec![1, 10]);
        std::thread::sleep(COALESCE + Duration::from_millis(50));
        assert_eq!(fs::read_to_string(p.join("frames")).unwrap().lines().count(), 10);
        fs::remove_file(&screen).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&log)
            .unwrap()
            .write_all(b"late USB detail\n")
            .unwrap();
        assert!(handle.join().unwrap().is_err());
        fs::remove_dir_all(p).unwrap();
    }
}
