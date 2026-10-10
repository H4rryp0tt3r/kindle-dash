/*
 * dash-status -- the single panel owner: a current-boot streaming console.
 * Watch logs/status with inotify, block while idle, and compose one conservative
 * screen update per frame. Output arriving during refresh is coalesced, not lost
 * in an unbounded repaint queue. No crates or framebuffer ABI changes.
 */
use std::ffi::{c_char, c_int};
#[cfg(test)]
use std::fs::OpenOptions;
use std::fs::{self, File};
#[cfg(test)]
use std::io::Write;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const LOG: &str = "/var/log/dash.log";
const RUN: &str = "/var/run";
const LIMIT: usize = 65536;
const WIDTH: usize = 43; // renderer allows 47; leave two-space margins
const LOG_ROWS: usize = 20;
const WATCHDOG: Duration = Duration::from_secs(20);
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
#[derive(Debug, PartialEq)]
struct Snapshot {
    logs: Vec<String>,
    usb: Status,
    ssh: Status,
    show_timeout: bool,
}
impl Snapshot {
    fn new(logs: Vec<String>, usb: Status, ssh: Status, expired: bool) -> Self {
        let show_timeout = expired && (!usb.terminal() || !ssh.terminal());
        Self {
            logs,
            usb,
            ssh,
            show_timeout,
        }
    }
}
#[derive(Default)]
struct MonitorState {
    last_snapshot: Option<Snapshot>,
    last_attempted_frame: String,
}
fn needs_compose(state: &MonitorState, snapshot: &Snapshot) -> bool {
    state.last_snapshot.as_ref() != Some(snapshot)
}
fn frame(release: &str, kernel: &str, uptime: &str, snapshot: &Snapshot) -> String {
    let header = vec![
        "Hello World!".to_string(),
        format!("Dash OS {}", printable(release.trim())),
        format!("uptime {uptime}s"),
        printable(kernel.trim()),
        String::new(),
    ];
    let services = [
        snapshot.usb.line("USB", snapshot.show_timeout),
        snapshot.ssh.line("SSH", snapshot.show_timeout),
    ]
    .into_iter()
    .flat_map(|line| wrap(&line));
    let connection = [
        "Kindle: 192.168.15.244".into(),
        "Mac: 192.168.15.201/24".into(),
        "Log available in diagnostics".into(),
        String::new(),
    ];
    let mut out = header;
    out.extend(services);
    out.extend(connection);
    let rows = LOG_ROWS.min(40usize.saturating_sub(out.len()));
    let start = snapshot.logs.len().saturating_sub(rows);
    out.extend_from_slice(&snapshot.logs[start..]);
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
fn observe(log: &Path, run: &Path, deadline: Instant) -> (Snapshot, bool) {
    let logs = log_tail(log).unwrap_or_default();
    let usb = read_status(&run.join("dash-usbnet.status"));
    let ssh = read_status(&run.join("dash-sshd.status"));
    let expired = Instant::now() >= deadline;
    (Snapshot::new(logs, usb, ssh, expired), expired)
}
fn render_frame(run: &Path, screen: &Path, frame: &str) -> io::Result<()> {
    let path = run.join("dash.frame");
    fs::write(&path, frame)?;
    let output = Command::new(screen).arg(path).output()?;
    if !output.status.success() {
        eprintln!("10-dash: screen failed: {}", output.status);
    }
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        eprintln!("10-dash: screen: {}", printable(line));
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
    let mut state = MonitorState::default();
    loop {
        watch.drain()?;
        let (snapshot, expired) = observe(log, run, deadline);
        // Intermediate stages are still shown with streamed log lines, but the
        // clock alone never makes the screen repaint continuously.
        if needs_compose(&state, &snapshot) {
            let uptime = fs::read_to_string("/proc/uptime").unwrap_or_default();
            let uptime = uptime.split('.').next().unwrap_or("?");
            let next = frame(release, kernel, uptime, &snapshot);
            if next != state.last_attempted_frame {
                render_frame(run, screen, &next)?;
                state.last_attempted_frame = next;
            }
            state.last_snapshot = Some(snapshot);
        }
        let timeout = if !expired {
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(i32::MAX as u128) as i32
        } else {
            -1
        };
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
            &Snapshot::new(logs, Status::pending(), Status::pending(), false),
        );
        assert!(f.contains("Hello World!"));
        assert!(f.contains("log 99"));
        assert!(!f.contains("log 0\n"));
        assert!(f.lines().count() <= 40);
        assert!(f.lines().all(|l| l.len() <= 45));
    }
    #[test]
    fn frame_layout_is_exact() {
        assert_eq!(
            frame(
                " test\n",
                " kernel\n",
                "3",
                &Snapshot::new(
                    vec!["new log".into()],
                    parse_status("ready\nUSB address"),
                    parse_status("disabled\nnot provisioned"),
                    false,
                ),
            ),
            concat!(
                "  Hello World!\n",
                "  Dash OS test\n",
                "  uptime 3s\n",
                "  kernel\n",
                "  \n",
                "  USB: configured - USB address\n",
                "  SSH: disabled - not provisioned\n",
                "  Kindle: 192.168.15.244\n",
                "  Mac: 192.168.15.201/24\n",
                "  Log available in diagnostics\n",
                "  \n",
                "  new log\n",
            )
        );
    }
    #[test]
    fn wrapped_status_leaves_only_remaining_rows_for_logs() {
        let status = parse_status(&format!("starting\n{}", "x".repeat(240)));
        let logs: Vec<_> = (0..30).map(|i| format!("log {i}")).collect();
        let snapshot = Snapshot::new(logs, status.clone(), status, false);
        let f = frame("test", "kernel", "3", &snapshot);
        assert_eq!(f.lines().count(), 40);
        assert!(!f.contains("  log 10\n"));
        assert!(f.contains("  log 11\n"));
        assert!(f.ends_with("  log 29\n"));
    }
    #[test]
    fn latest_boot_filters_both_diagnostic_sources() {
        assert_eq!(
            boot_lines(concat!(
                "old\n0s stage1: start, mounts up\nold detail\n",
                "0s stage1: start, mounts up\n",
                "10-dash: screen failed\ndash-status: failure\n",
                "new\n\nlast\tline",
            )),
            vec!["0s stage1: start, mounts up", "new", "", "last line"]
        );
    }
    #[test]
    fn composition_depends_on_complete_snapshot() {
        let pending = || {
            Snapshot::new(
                vec!["log".into()],
                Status::pending(),
                Status::pending(),
                false,
            )
        };
        let mut state = MonitorState::default();
        assert!(needs_compose(&state, &pending()));
        state.last_snapshot = Some(pending());
        assert!(!needs_compose(&state, &pending()));
        let mut logs = pending();
        logs.logs.push("another line".into());
        assert!(needs_compose(&state, &logs));
        let mut usb = pending();
        usb.usb = parse_status("ready\nconfigured");
        assert!(needs_compose(&state, &usb));
        let mut ssh = pending();
        ssh.ssh = parse_status("disabled\nnot provisioned");
        assert!(needs_compose(&state, &ssh));
        let timed_out = Snapshot::new(
            vec!["log".into()],
            Status::pending(),
            Status::pending(),
            true,
        );
        assert!(needs_compose(&state, &timed_out));
    }
    #[test]
    fn timeout_changes_only_pending_display_and_late_status_recovers() {
        let terminal = |expired| {
            Snapshot::new(
                vec![],
                parse_status("ready\nconfigured"),
                parse_status("disabled\nno key"),
                expired,
            )
        };
        assert_eq!(terminal(false), terminal(true));
        let mut state = MonitorState {
            last_snapshot: Some(terminal(false)),
            ..MonitorState::default()
        };
        assert!(!needs_compose(&state, &terminal(true)));
        let timed_out = Snapshot::new(vec![], Status::pending(), Status::pending(), true);
        assert!(needs_compose(&state, &timed_out));
        assert!(frame("test", "kernel", "3", &timed_out).contains("TIMED OUT"));
        state.last_snapshot = Some(timed_out);
        assert!(needs_compose(&state, &terminal(true)));
        assert!(!frame("test", "kernel", "3", &terminal(true)).contains("TIMED OUT"));
    }
    #[test]
    fn changed_offscreen_input_can_have_identical_frame() {
        let make = |first: &str| {
            let mut logs = vec![first.into()];
            logs.extend((0..20).map(|i| format!("log {i}")));
            Snapshot::new(logs, Status::pending(), Status::pending(), false)
        };
        let old = make("old hidden line");
        let last_attempted_frame = frame("test", "kernel", "3", &old);
        let mut state = MonitorState {
            last_snapshot: Some(old),
            last_attempted_frame,
        };
        let next = make("new hidden line");
        assert!(needs_compose(&state, &next));
        assert_eq!(
            frame("test", "kernel", "3", &next),
            state.last_attempted_frame
        );
        assert_ne!(
            frame("test", "kernel", "4", &next),
            state.last_attempted_frame
        );
        state.last_snapshot = Some(next);
        assert!(!needs_compose(&state, &make("new hidden line")));
    }
    #[test]
    fn renderer_nonzero_exit_is_completed_attempt_but_io_errors_propagate() {
        let p = std::env::temp_dir().join(format!("dash-render-test-{}", std::process::id()));
        fs::create_dir(&p).unwrap();
        let screen = p.join("screen");
        fs::write(&screen, "#!/bin/sh\nprintf 'bad\\toutput\\n' >&2\nexit 1\n").unwrap();
        fs::set_permissions(&screen, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(render_frame(&p, &screen, "frame").is_ok());
        assert_eq!(fs::read_to_string(p.join("dash.frame")).unwrap(), "frame");
        assert!(render_frame(&p, &p.join("missing-screen"), "next").is_err());
        assert!(render_frame(&p.join("missing-run"), &screen, "next").is_err());
        fs::remove_dir_all(p).unwrap();
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
        // Stop the monitor after the fourth frame by replacing the executable
        // with a missing path. The clock and writes are test-only, never device.
        fs::write(&screen, format!("#!/bin/sh\nprintf 'frame\\n' >> '{}/frames'\ncp \"$1\" '{}/latest'\nprintf '10-dash: renderer output\\n' >> '{}'\n",p.display(),p.display(),log.display())).unwrap();
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
                Duration::from_millis(100),
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
