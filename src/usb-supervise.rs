/*
 * usb-supervise -- one bounded USB setup attempt per boot, never a restart loop.
 * Kernel-facing work runs in a separate process group. Its exit/signal and last
 * stage survive in the log; a failed/timed-out attempt stays latched in tmpfs.
 * Killing a process cannot guarantee recovery from an uninterruptible driver.
 */
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(15);
static STOP: AtomicBool = AtomicBool::new(false);
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
const O_NOFOLLOW: i32 = 0o100000;
#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
const O_NOFOLLOW: i32 = 0o400000;
const O_CLOEXEC: i32 = 0o2000000;
extern "C" {
    fn poll(fds: *mut std::ffi::c_void, count: usize, timeout: i32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn setpgid(pid: i32, pgid: i32) -> i32;
    fn signal(number: i32, handler: usize) -> usize;
    fn flock(fd: i32, operation: i32) -> i32;
}
extern "C" fn stop(_: i32) {
    STOP.store(true, Ordering::SeqCst);
}
fn pause(ms: i32) -> io::Result<()> {
    if unsafe { poll(std::ptr::null_mut(), 0, ms) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}
fn park() -> io::Result<()> {
    // Bounded poll also handles a stop arriving just before we enter the wait.
    while !STOP.load(Ordering::SeqCst) {
        pause(1000)?;
    }
    Ok(())
}
fn group(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            if setpgid(0, 0) < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}
fn terminate(child: &mut Child) -> io::Result<()> {
    // A child is not reaped before signals, so its PID cannot be reused here.
    let pid = child.id() as i32;
    unsafe {
        kill(-pid, 15);
        kill(-pid, 9);
    }
    // Do not call wait(): a task stuck in a kernel write may never return.
    let _ = child.try_wait()?;
    Ok(())
}
#[derive(Debug)]
enum Outcome {
    Exit(ExitStatus),
    Timeout,
    Stopped,
}
fn await_worker(child: &mut Child, timeout: Duration, stopped: &AtomicBool) -> io::Result<Outcome> {
    monitor_worker(child, timeout, stopped, || Ok(()))
}
fn monitor_worker<F: FnMut() -> io::Result<()>>(
    child: &mut Child,
    timeout: Duration,
    stopped: &AtomicBool,
    mut progress: F,
) -> io::Result<Outcome> {
    let deadline = Instant::now() + timeout;
    loop {
        if stopped.load(Ordering::SeqCst) {
            return Ok(Outcome::Stopped);
        }
        if Instant::now() >= deadline {
            return Ok(Outcome::Timeout);
        }
        progress()?;
        if let Some(status) = child.try_wait()? {
            return Ok(Outcome::Exit(status));
        }
        pause(25)?;
    }
}
fn exit_reason(status: ExitStatus) -> String {
    match status.signal() {
        Some(signal) => format!("worker killed by signal {signal}"),
        None => format!("worker exited rc={}", status.code().unwrap_or(-1)),
    }
}
fn bounded(path: &Path, limit: u64) -> io::Result<String> {
    let mut text = String::new();
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW | O_CLOEXEC | 0o4000)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "USB state is not a regular file",
        ));
    }
    file.take(limit + 1).read_to_string(&mut text)?;
    if text.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized USB state",
        ));
    }
    Ok(text)
}
fn clean(text: &str) -> String {
    text.bytes()
        .filter(|b| (32..=126).contains(b))
        .take(160)
        .map(char::from)
        .collect()
}
struct State {
    run: PathBuf,
    log: PathBuf,
}
impl State {
    fn path(&self, name: &str) -> PathBuf {
        self.run.join(name)
    }
    fn log(&self, text: &str) -> io::Result<()> {
        let uptime = fs::read_to_string("/proc/uptime").unwrap_or_default();
        let secs = uptime.split('.').next().unwrap_or("?");
        writeln!(
            OpenOptions::new().append(true).open(&self.log)?,
            "{secs}s  20-usbnet: {}",
            clean(text)
        )
    }
    fn write(&self, name: &str, text: &str) -> io::Result<()> {
        let path = self.path(name);
        let temporary = self.path(&format!(".{name}.supervisor-new"));
        let mut created = false;
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(O_NOFOLLOW | O_CLOEXEC)
                .open(&temporary)?;
            created = true;
            file.write_all(text.as_bytes())?;
            fs::rename(&temporary, &path)
        })();
        if created && result.is_err() {
            // Remove only this invocation's temporary file, not stale or foreign state.
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    fn remove(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.path(name)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
    fn claim(&self) -> io::Result<Option<File>> {
        let meta = fs::symlink_metadata(&self.run)?;
        if !meta.is_dir() || meta.uid() != 0 || meta.gid() != 0 || meta.mode() & 0o7022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe USB runtime directory",
            ));
        }
        let mut latch = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(O_NOFOLLOW | O_CLOEXEC)
            .open(self.path("dash-usbnet-attempt"))
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let existing = OpenOptions::new()
                    .read(true)
                    .custom_flags(O_NOFOLLOW | O_CLOEXEC | 0o4000)
                    .open(self.path("dash-usbnet-attempt"))?;
                if unsafe { flock(existing.as_raw_fd(), 2 | 4) } < 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "USB supervisor already running",
                    ));
                }
                if !existing.metadata()?.is_file() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid USB attempt latch",
                    ));
                }
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        if unsafe { flock(latch.as_raw_fd(), 2 | 4) } < 0 {
            return Err(io::Error::last_os_error());
        }
        latch.write_all(b"attempted\n")?;
        Ok(Some(latch))
    }
    fn progress(&self) -> io::Result<()> {
        let text = match bounded(&self.path("dash-usbnet-worker.status"), 256) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut lines = text.lines();
        let phase = lines.next().unwrap_or("");
        let detail = lines.next().unwrap_or("");
        if !["starting", "failed"].contains(&phase) || lines.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid worker progress",
            ));
        }
        let public = format!("{phase}\n{}\n", clean(detail));
        if bounded(&self.path("dash-usbnet.status"), 256).ok().as_ref() != Some(&public) {
            self.write("dash-usbnet.status", &public)?;
        }
        Ok(())
    }
    fn stage(&self) -> String {
        bounded(&self.path("dash-usbnet.status"), 256)
            .ok()
            .and_then(|s| s.lines().nth(1).map(clean))
            .unwrap_or_else(|| "unknown stage".into())
    }
    fn fail(&self, reason: &str) -> io::Result<()> {
        self.remove("dash-usbnet-ready")?;
        let message = format!("{}; {}", clean(reason), self.stage());
        self.write("dash-usbnet-failed", &format!("{message}\n"))?;
        self.write(
            "dash-usbnet.status",
            &format!("failed\n{}\n", clean(&message)),
        )?;
        self.log(&format!("unavailable: {message}"))
    }
    fn ready(&self) -> io::Result<()> {
        if bounded(&self.path("dash-usbnet-worker.status"), 256)? != "starting\nsetup complete\n" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "worker exited without verified setup completion",
            ));
        }
        self.write("dash-usbnet.status", "ready\n192.168.15.244/24\n")?;
        if let Err(error) = self.write("dash-usbnet-ready", "192.168.15.244/24\n") {
            let _ = self.fail("could not publish readiness");
            return Err(error);
        }
        self.log("ready; Mac USB Ethernet address is 192.168.15.201/24")
    }
}
fn kernel_log(state: &State) -> io::Result<()> {
    state.log("kernel diagnostics follow (last 80 lines)")?;
    let out = OpenOptions::new().append(true).open(&state.log)?;
    let err = out.try_clone()?;
    let mut command = Command::new("/bin/busybox");
    command
        .args(["sh", "-c", "set -o pipefail; /bin/busybox dmesg | /bin/busybox tail -n 80 | /bin/busybox tail -c 8192"])
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    group(&mut command);
    let mut child = command.spawn()?;
    let stopped = AtomicBool::new(false);
    match await_worker(&mut child, Duration::from_secs(1), &stopped)? {
        Outcome::Exit(s) if s.success() => Ok(()),
        Outcome::Exit(s) => Err(io::Error::new(io::ErrorKind::Other, exit_reason(s))),
        _ => {
            terminate(&mut child)?;
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "kernel log capture timed out",
            ))
        }
    }
}
#[derive(Debug)]
struct Attempt {
    _owner: Option<File>,
    #[allow(dead_code)] // asserted by tests; production holds ownership while parked
    ready: bool,
}
fn supervise(state: &State, command: &mut Command, timeout: Duration) -> io::Result<Attempt> {
    let claim = match state.claim()? {
        Some(file) => file,
        None => {
            // No retry even if prior child survived the supervisor. Preserve a
            // stable existing failure rather than appending repeated messages.
            if !state.path("dash-usbnet-failed").exists() {
                state.fail("setup already attempted this boot; no retry")?;
            }
            return Ok(Attempt {
                _owner: None,
                ready: false,
            });
        }
    };
    state.remove("dash-usbnet-ready")?;
    state.remove("dash-usbnet-failed")?;
    state.remove("dash-usbnet-worker.status")?;
    state.write("dash-usbnet.status", "starting\nlaunching setup worker\n")?;
    if STOP.load(Ordering::SeqCst) {
        state.write("dash-usbnet.status", "stopped\nservice stopped\n")?;
        return Ok(Attempt {
            _owner: Some(claim),
            ready: false,
        });
    }
    state.log("one setup attempt; no forced role writes")?;
    group(command);
    let out = OpenOptions::new().append(true).open(&state.log)?;
    command
        .stdout(Stdio::from(out.try_clone()?))
        .stderr(Stdio::from(out));
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            state.fail(&format!("could not launch worker: {error}"))?;
            return Ok(Attempt {
                _owner: Some(claim),
                ready: false,
            });
        }
    };
    state
        .write("dash-usbnet-worker-pid", &format!("{}\n", child.id()))
        .map_err(|e| {
            let _ = terminate(&mut child);
            e
        })?;
    let outcome = monitor_worker(&mut child, timeout, &STOP, || state.progress());
    match outcome {
        Ok(Outcome::Exit(status)) if status.success() => {
            state.remove("dash-usbnet-worker-pid")?;
            if let Err(error) = state.ready() {
                state.fail(&format!("readiness failed: {error}"))?;
                return Ok(Attempt {
                    _owner: Some(claim),
                    ready: false,
                });
            }
            Ok(Attempt {
                _owner: Some(claim),
                ready: true,
            })
        }
        Ok(Outcome::Stopped) => {
            terminate(&mut child)?;
            state.remove("dash-usbnet-ready")?;
            state.write("dash-usbnet.status", "stopped\nservice stopped\n")?;
            Ok(Attempt {
                _owner: Some(claim),
                ready: false,
            })
        }
        failure => {
            let reason = match failure {
                Ok(Outcome::Exit(s)) => {
                    state.remove("dash-usbnet-worker-pid")?;
                    exit_reason(s)
                }
                other => {
                    // Only signal an unreaped worker: a reaped PID may be reused.
                    let _ = terminate(&mut child);
                    match other {
                        Ok(Outcome::Timeout) => "setup deadline exceeded; no retry".into(),
                        Err(e) => format!("worker supervision failed: {e}"),
                        _ => unreachable!(),
                    }
                }
            };
            // Signals cannot fix a driver deadlock. The latch forbids another worker.
            state.fail(&reason)?;
            if let Err(error) = kernel_log(state) {
                state.log(&format!("kernel diagnostics unavailable: {error}"))?;
            }
            Ok(Attempt {
                _owner: Some(claim),
                ready: false,
            })
        }
    }
}
fn run() -> io::Result<()> {
    if std::env::args_os().len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: usb-supervise",
        ));
    }
    for number in [1, 2, 15] {
        if unsafe { signal(number, stop as usize) } == usize::MAX {
            return Err(io::Error::last_os_error());
        }
    }
    let state = State {
        run: "/var/run".into(),
        log: "/var/log/dash.log".into(),
    };
    let mut command = Command::new("/bin/busybox");
    command.args(["sh", "/service/20-usbnet/run", "setup"]);
    let attempt = match supervise(&state, &mut command, DEADLINE) {
        Ok(attempt) => Some(attempt),
        Err(error) => {
            eprintln!("20-usbnet: supervision unavailable: {error}");
            if error.kind() == io::ErrorKind::WouldBlock {
                // A second instance must neither overwrite the active owner nor
                // repeatedly restart through runsv. Its latch is already active.
                return park();
            }
            let _ = state.fail("supervision state error; no retry");
            None
        }
    };
    // Hold the owner lock throughout parking, including after successful setup.
    let _attempt = attempt;
    park()?;
    state.remove("dash-usbnet-ready")?;
    state.write("dash-usbnet.status", "stopped\nservice stopped\n")
}
fn main() {
    if std::env::args_os().len() != 1 {
        eprintln!("usage: usb-supervise");
        std::process::exit(2);
    }
    if let Err(error) = run() {
        eprintln!("20-usbnet: supervisor error: {error}");
        // Missing/broken runtime storage must not create a runsv log/restart storm.
        let _ = park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::AtomicUsize;
    static ID: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "usb-supervise-test-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir(&p).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(p.join("log"), b"").unwrap();
            Self(p)
        }
        fn state(&self) -> State {
            State {
                run: self.0.clone(),
                log: self.0.join("log"),
            }
        }
        fn worker(&self, body: &str) -> Command {
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(body).current_dir(&self.0);
            c
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn claim_allows_only_one_attempt_and_rejects_symlinks() {
        let f = Fixture::new();
        assert!(f.state().claim().unwrap().is_some());
        assert!(f.state().claim().unwrap().is_none());
        let link = f.0.join("link");
        symlink(&f.0, &link).unwrap();
        let s = State {
            run: link,
            log: f.0.join("log"),
        };
        assert!(s.claim().is_err());
    }
    #[test]
    fn unsafe_runtime_permissions_rejected() {
        let f = Fixture::new();
        fs::set_permissions(&f.0, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(f.state().claim().is_err());
    }
    #[test]
    fn success_requires_verified_completion_then_atomic_readiness() {
        let f = Fixture::new();
        let mut c = f.worker("printf 'starting\\nsetup complete\\n' >dash-usbnet-worker.status");
        assert!(
            supervise(&f.state(), &mut c, Duration::from_secs(2))
                .unwrap()
                .ready
        );
        assert_eq!(
            fs::read_to_string(f.0.join("dash-usbnet-ready")).unwrap(),
            "192.168.15.244/24\n"
        );
        assert!(bounded(&f.0.join("dash-usbnet.status"), 256)
            .unwrap()
            .starts_with("ready\n"));
    }
    #[test]
    fn exit_zero_without_completion_does_not_announce_success() {
        let f = Fixture::new();
        let mut c = f.worker("exit 0");
        assert!(
            !supervise(&f.state(), &mut c, Duration::from_secs(1))
                .unwrap()
                .ready
        );
        assert!(!f.0.join("dash-usbnet-ready").exists());
        assert!(f.0.join("dash-usbnet-failed").exists());
    }
    #[test]
    fn deadline_stops_worker_without_unbounded_wait_or_second_attempt() {
        let f = Fixture::new();
        let mut c = f.worker("exec sleep 30");
        let start = Instant::now();
        assert!(
            !supervise(&f.state(), &mut c, Duration::from_millis(50))
                .unwrap()
                .ready
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        let mut retry = f.worker("touch retried");
        assert!(
            !supervise(&f.state(), &mut retry, Duration::from_secs(1))
                .unwrap()
                .ready
        );
        assert!(!f.0.join("retried").exists());
        assert!(!f.0.join("dash-usbnet-ready").exists());
    }
    #[test]
    fn normal_failure_keeps_code_and_last_stage() {
        let f = Fixture::new();
        let mut c = f
            .worker("printf 'starting\\nloading arcotg_udc\\n' >dash-usbnet-worker.status; exit 7");
        assert!(
            !supervise(&f.state(), &mut c, Duration::from_secs(1))
                .unwrap()
                .ready
        );
        let log = fs::read_to_string(f.0.join("log")).unwrap();
        assert!(log.contains("rc=7"));
        assert!(log.contains("loading arcotg_udc"));
    }
    #[test]
    fn fatal_signals_record_signal_and_stop_restart_storm() {
        for sig in ["SEGV", "KILL"] {
            let f = Fixture::new();
            let mut c = f.worker(&format!("ulimit -c 0; kill -{sig} $$"));
            assert!(
                !supervise(&f.state(), &mut c, Duration::from_secs(1))
                    .unwrap()
                    .ready
            );
            let before = fs::read_to_string(f.0.join("log")).unwrap();
            assert!(before.contains("killed by signal"));
            let mut retry = f.worker("touch retried");
            assert!(
                !supervise(&f.state(), &mut retry, Duration::from_secs(1))
                    .unwrap()
                    .ready
            );
            assert_eq!(before, fs::read_to_string(f.0.join("log")).unwrap());
            assert!(!f.0.join("retried").exists());
        }
    }
    #[test]
    fn already_claimed_live_worker_never_retried() {
        let f = Fixture::new();
        f.state().claim().unwrap();
        fs::write(
            f.0.join("dash-usbnet-worker-pid"),
            std::process::id().to_string(),
        )
        .unwrap();
        let mut c = f.worker("touch retried");
        assert!(
            !supervise(&f.state(), &mut c, Duration::from_secs(1))
                .unwrap()
                .ready
        );
        assert!(!f.0.join("retried").exists());
    }
    #[test]
    fn missing_worker_fails_once() {
        let f = Fixture::new();
        let mut c = Command::new(f.0.join("missing"));
        assert!(
            !supervise(&f.state(), &mut c, Duration::from_secs(1))
                .unwrap()
                .ready
        );
        assert!(fs::read_to_string(f.0.join("log"))
            .unwrap()
            .contains("could not launch worker"));
    }
    #[test]
    fn failed_readiness_write_removes_stale_ready_and_is_terminal() {
        let f = Fixture::new();
        fs::write(
            f.0.join("dash-usbnet-worker.status"),
            "starting\nsetup complete\n",
        )
        .unwrap();
        fs::write(f.0.join("dash-usbnet-ready"), "stale").unwrap();
        fs::write(f.0.join(".dash-usbnet-ready.supervisor-new"), "blocked").unwrap();
        assert!(f.state().ready().is_err());
        assert!(!f.0.join("dash-usbnet-ready").exists());
        assert!(f.0.join("dash-usbnet-failed").exists());
    }
    #[test]
    fn bounded_state_and_failed_atomic_update() {
        let f = Fixture::new();
        fs::write(f.0.join("large"), vec![b'x'; 300]).unwrap();
        assert!(bounded(&f.0.join("large"), 256).is_err());
        fs::write(f.0.join("dash-usbnet.status"), "original").unwrap();
        fs::write(f.0.join(".dash-usbnet.status.supervisor-new"), "existing").unwrap();
        assert!(f.state().write("dash-usbnet.status", "new").is_err());
        assert_eq!(
            fs::read_to_string(f.0.join("dash-usbnet.status")).unwrap(),
            "original"
        );
    }
    #[test]
    fn stopped_worker_is_not_reported_as_success() {
        let f = Fixture::new();
        let mut cmd = f.worker("exec sleep 30");
        group(&mut cmd);
        let mut c = cmd.spawn().unwrap();
        let stopped = AtomicBool::new(true);
        assert!(matches!(
            await_worker(&mut c, Duration::from_secs(1), &stopped).unwrap(),
            Outcome::Stopped
        ));
        terminate(&mut c).unwrap();
        let _ = c.wait();
    }
    #[test]
    fn active_owner_is_not_overwritten_by_second_supervisor() {
        let f = Fixture::new();
        let _owner = f.state().claim().unwrap().unwrap();
        fs::write(f.0.join("dash-usbnet.status"), "starting\nactive worker\n").unwrap();
        let mut second = f.worker("touch retried");
        let error = supervise(&f.state(), &mut second, Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(
            fs::read_to_string(f.0.join("dash-usbnet.status")).unwrap(),
            "starting\nactive worker\n"
        );
        assert!(!f.0.join("retried").exists());
    }
    #[test]
    fn unreadable_and_symlink_status_never_creates_readiness() {
        let f = Fixture::new();
        assert!(f.state().ready().is_err());
        fs::write(f.0.join("target"), "starting\nsetup complete\n").unwrap();
        symlink("target", f.0.join("dash-usbnet-worker.status")).unwrap();
        assert!(f.state().ready().is_err());
        assert!(!f.0.join("dash-usbnet-ready").exists());
    }
    #[test]
    fn stop_request_wins_over_elapsed_deadline() {
        // Test the pure wait decision without changing the process-global handler.
        let f = Fixture::new();
        let mut cmd = f.worker("exec sleep 30");
        group(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let flag = AtomicBool::new(true);
        assert!(matches!(
            await_worker(&mut child, Duration::ZERO, &flag).unwrap(),
            Outcome::Stopped
        ));
        terminate(&mut child).unwrap();
        let _ = child.wait();
    }
    #[test]
    fn timeout_kills_worker_process_group_descendants() {
        let f = Fixture::new();
        let mut c = f.worker("sleep 30 & echo $! >descendant; wait");
        assert!(
            !supervise(&f.state(), &mut c, Duration::from_millis(100))
                .unwrap()
                .ready
        );
        let pid: i32 = fs::read_to_string(f.0.join("descendant"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let zombie = status
                .rsplit_once(')')
                .map(|(_, tail)| tail.trim_start().starts_with('Z'))
                .unwrap_or(false);
            if status.is_empty() || zombie {
                break;
            }
            assert!(Instant::now() < deadline, "descendant survived timeout");
            pause(10).unwrap();
        }
    }
    #[test]
    fn worker_cannot_complete_after_supervisor_timeout() {
        let f = Fixture::new();
        let mut c =
            f.worker("sleep 30; printf 'starting\\nsetup complete\\n' >dash-usbnet-worker.status");
        assert!(
            !supervise(&f.state(), &mut c, Duration::from_millis(50))
                .unwrap()
                .ready
        );
        let terminal = fs::read_to_string(f.0.join("dash-usbnet.status")).unwrap();
        pause(100).unwrap();
        assert_eq!(
            fs::read_to_string(f.0.join("dash-usbnet.status")).unwrap(),
            terminal
        );
        assert!(terminal.starts_with("failed\n"));
        assert!(!f.0.join("dash-usbnet-ready").exists());
    }
    #[test]
    fn worker_pid_write_failure_terminates_child() {
        let f = Fixture::new();
        fs::write(
            f.0.join(".dash-usbnet-worker-pid.supervisor-new"),
            "blocked",
        )
        .unwrap();
        let mut c = f.worker("sleep 1; touch escaped");
        assert!(supervise(&f.state(), &mut c, Duration::from_secs(2)).is_err());
        pause(1100).unwrap();
        assert!(!f.0.join("escaped").exists());
        assert!(!f.0.join("dash-usbnet-ready").exists());
    }
    #[test]
    fn invalid_worker_status_is_terminal_without_readiness() {
        for body in [
            "printf 'ready\\nfake success\\n' >dash-usbnet-worker.status; exec sleep 30",
            "printf 'starting\\nvalid\\nextra\\n' >dash-usbnet-worker.status; exec sleep 30",
            "head -c 300 /dev/zero | tr '\\000' x >dash-usbnet-worker.status; exec sleep 30",
        ] {
            let f = Fixture::new();
            let mut c = f.worker(body);
            assert!(
                !supervise(&f.state(), &mut c, Duration::from_secs(1))
                    .unwrap()
                    .ready
            );
            assert!(f.0.join("dash-usbnet-failed").exists());
            assert!(!f.0.join("dash-usbnet-ready").exists());
        }
    }
    #[test]
    fn successful_attempt_retains_owner_until_dropped() {
        let f = Fixture::new();
        let mut c = f.worker("printf 'starting\\nsetup complete\\n' >dash-usbnet-worker.status");
        let owner = supervise(&f.state(), &mut c, Duration::from_secs(1)).unwrap();
        assert!(owner.ready);
        let mut duplicate = f.worker("touch retried");
        assert_eq!(
            supervise(&f.state(), &mut duplicate, Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(f.0.join("dash-usbnet-ready").exists());
        drop(owner);
        let mut restarted = f.worker("touch retried");
        assert!(
            !supervise(&f.state(), &mut restarted, Duration::from_secs(1))
                .unwrap()
                .ready
        );
        assert!(!f.0.join("retried").exists());
        assert!(!f.0.join("dash-usbnet-ready").exists());
    }
    #[test]
    fn late_worker_status_cannot_overwrite_terminal_public_state() {
        let f = Fixture::new();
        f.state().fail("timeout").unwrap();
        let terminal = fs::read_to_string(f.0.join("dash-usbnet.status")).unwrap();
        fs::write(
            f.0.join("dash-usbnet-worker.status"),
            "starting\nsetup complete\n",
        )
        .unwrap();
        let mut retry = f.worker("touch retried");
        f.state().claim().unwrap();
        assert!(
            !supervise(&f.state(), &mut retry, Duration::from_secs(1))
                .unwrap()
                .ready
        );
        assert_eq!(
            fs::read_to_string(f.0.join("dash-usbnet.status")).unwrap(),
            terminal
        );
        assert!(!f.0.join("retried").exists());
    }
    #[test]
    fn exit_reason_distinguishes_codes_and_signals() {
        assert_eq!(
            exit_reason(ExitStatus::from_raw(7 << 8)),
            "worker exited rc=7"
        );
        assert_eq!(
            exit_reason(ExitStatus::from_raw(11)),
            "worker killed by signal 11"
        );
    }
}
