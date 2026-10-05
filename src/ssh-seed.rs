/*
 * ssh-seed -- initialise SSH entropy once per boot on Linux 3.0.35.
 *
 * A separately provisioned, unique 64-byte host seed is trusted entropy. The
 * seed is durably removed BEFORE crediting it to the kernel: interruption
 * leaves SSH unavailable rather than allowing that seed to be reused. A fresh
 * seed is committed before the tmpfs success marker. No bytes are ever logged.
 *
 * No crates and no C sources; the small Linux ABI below is declared by hand.
 */
use std::ffi::{c_char, c_int, c_ulong};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

const STATE: &str = "/var/lib/dash-ssh";
const RUN: &str = "/var/run";
const SEED: &[u8] = b"seed\0";
const NEXT: &[u8] = b".seed-next\0";
const MARKER: &[u8] = b"dash-ssh-seeded\0";
const MARKER_CONTENT: &[u8] = b"seeded\n";
const SEED_LEN: usize = 64;
const O_WRONLY: c_int = 1;
const O_CREAT: c_int = 0o100;
const O_EXCL: c_int = 0o200;
const O_NONBLOCK: c_int = 0o4000;
// ARM's Linux fcntl values differ from the asm-generic/x86 values. Native
// tests also run in an aarch64 container on Apple Silicon.
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
const O_DIRECTORY: c_int = 0o40000;
#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
const O_DIRECTORY: c_int = 0o200000;
#[cfg(any(target_arch = "arm", target_arch = "aarch64"))]
const O_NOFOLLOW: c_int = 0o100000;
#[cfg(not(any(target_arch = "arm", target_arch = "aarch64")))]
const O_NOFOLLOW: c_int = 0o400000;
const O_CLOEXEC: c_int = 0o2000000;
const LOCK_EX: c_int = 2;
const LOCK_NB: c_int = 4;
const RNDADDENTROPY: c_ulong = 0x4008_5203;

extern "C" {
    fn geteuid() -> u32;
    fn openat(dirfd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn unlinkat(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int;
    fn renameat(oldfd: c_int, old: *const c_char, newfd: c_int, new: *const c_char) -> c_int;
    fn flock(fd: c_int, operation: c_int) -> c_int;
    #[cfg(test)]
    fn mkfifo(path: *const c_char, mode: u32) -> c_int;
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

#[repr(C)]
struct EntropyPool {
    entropy_count: c_int,
    buf_size: c_int,
    data: [u8; SEED_LEN],
}
const _: () = assert!(std::mem::size_of::<EntropyPool>() == 72);

struct Seed([u8; SEED_LEN]);

fn erase(bytes: &mut [u8]) {
    for byte in bytes {
        // Unlike fill(), a volatile store cannot be removed as a dead write.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}
impl Drop for Seed {
    fn drop(&mut self) {
        erase(&mut self.0);
    }
}
impl Drop for EntropyPool {
    fn drop(&mut self) {
        erase(&mut self.data);
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn owner_mode(uid: u32, gid: u32, mode: u32, expected: u32) -> io::Result<()> {
    if uid != 0 || gid != 0 || mode & 0o7777 != expected {
        return Err(invalid("expected root ownership and secure permissions"));
    }
    Ok(())
}

fn private_file(meta: &Metadata, length: u64) -> io::Result<()> {
    if !meta.is_file() || meta.len() != length || meta.nlink() != 1 {
        return Err(invalid("expected single-link regular file with exact length"));
    }
    owner_mode(meta.uid(), meta.gid(), meta.mode(), 0o600)
}

fn directory(path: &Path, private: bool) -> io::Result<File> {
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        .open(path)?;
    let meta = dir.metadata()?;
    if private {
        owner_mode(meta.uid(), meta.gid(), meta.mode(), 0o700)?;
    } else if meta.uid() != 0 || meta.gid() != 0 || meta.mode() & 0o7022 != 0 {
        return Err(invalid("runtime directory must be root-owned and not writable by others"));
    }
    Ok(dir)
}

fn trusted_ancestors(path: &Path) -> io::Result<()> {
    let mut current = std::path::PathBuf::from("/");
    directory(&current, false)?;
    for component in path.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(name) => current.push(name),
            _ => return Err(invalid("expected absolute directory path")),
        }
        directory(&current, false)?;
    }
    Ok(())
}

fn relative_file(dir: &File, name: &[u8], flags: c_int) -> io::Result<File> {
    let fd = unsafe {
        openat(
            dir.as_raw_fd(),
            name.as_ptr().cast(),
            flags | O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC,
            0o600 as c_int,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn unlink(dir: &File, name: &[u8]) -> io::Result<()> {
    if unsafe { unlinkat(dir.as_raw_fd(), name.as_ptr().cast(), 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn rename(dir: &File, old: &[u8], new: &[u8]) -> io::Result<()> {
    if unsafe {
        renameat(dir.as_raw_fd(), old.as_ptr().cast(), dir.as_raw_fd(), new.as_ptr().cast())
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn lock(dir: &File) -> io::Result<()> {
    // Separate open file descriptions contend even within the same process.
    // The lock releases on process death without a persistent stale lock file.
    if unsafe { flock(dir.as_raw_fd(), LOCK_EX | LOCK_NB) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_seed(state: &File) -> io::Result<Seed> {
    let mut file = relative_file(state, SEED, 0)?;
    private_file(&file.metadata()?, SEED_LEN as u64)?;
    let mut seed = Seed([0; SEED_LEN]);
    file.read_exact(&mut seed.0)?;
    Ok(seed)
}

fn marker_present(run: &File) -> io::Result<bool> {
    let mut file = match relative_file(run, MARKER, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    private_file(&file.metadata()?, MARKER_CONTENT.len() as u64)?;
    let mut contents = [0; MARKER_CONTENT.len()];
    file.read_exact(&mut contents)?;
    if contents != MARKER_CONTENT {
        return Err(invalid("invalid boot marker"));
    }
    Ok(true)
}

trait Entropy {
    fn credit(&mut self, seed: &Seed) -> io::Result<()>;
    fn fresh(&mut self) -> io::Result<Seed>;
}

struct KernelEntropy {
    random: File,
    urandom: File,
}

fn random_device(path: &str, device: u64, write: bool) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .custom_flags(O_NOFOLLOW | O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_char_device()
        || meta.uid() != 0
        || meta.gid() != 0
        || meta.rdev() != device
    {
        return Err(invalid("expected genuine root-owned Linux random device"));
    }
    Ok(file)
}

impl KernelEntropy {
    fn open() -> io::Result<Self> {
        Ok(Self {
            random: random_device("/dev/random", (1 << 8) | 8, true)?,
            urandom: random_device("/dev/urandom", (1 << 8) | 9, false)?,
        })
    }
}

fn entropy_pool(seed: &Seed) -> EntropyPool {
    EntropyPool {
        entropy_count: (SEED_LEN * 8) as c_int,
        buf_size: SEED_LEN as c_int,
        data: seed.0,
    }
}

impl Entropy for KernelEntropy {
    fn credit(&mut self, seed: &Seed) -> io::Result<()> {
        let pool = entropy_pool(seed);
        if unsafe { ioctl(self.random.as_raw_fd(), RNDADDENTROPY, &pool as *const _) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn fresh(&mut self) -> io::Result<Seed> {
        let mut seed = Seed([0; SEED_LEN]);
        self.urandom.read_exact(&mut seed.0)?;
        Ok(seed)
    }
}

fn write_private(dir: &File, name: &[u8], data: &[u8]) -> io::Result<()> {
    let mut file = relative_file(dir, name, O_WRONLY | O_CREAT | O_EXCL)?;
    let result = (|| {
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        private_file(&file.metadata()?, 0)?;
        file.write_all(data)?;
        file.sync_all()
    })();
    if result.is_err() {
        // Only remove the exclusive file this invocation created, never an
        // existing file or symlink rejected by openat().
        let _ = unlink(dir, name);
        let _ = dir.sync_all();
    }
    result
}

fn initialise<E: Entropy>(state: &File, run: &File, entropy: &mut E) -> io::Result<bool> {
    lock(state)?;
    let seed = read_seed(state)?;
    if marker_present(run)? {
        return Ok(false);
    }
    match relative_file(state, NEXT, 0) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        _ => return Err(invalid("unfinished seed rotation; reprovision before enabling SSH")),
    }

    // A power cut after removal but before replacement requires reprovisioning.
    // Never retain a replayable seed for a retry after entropy has been credited.
    unlink(state, SEED)?;
    state.sync_all()?;
    entropy.credit(&seed)?;
    let fresh = entropy.fresh()?;
    write_private(state, NEXT, &fresh.0)?;
    rename(state, NEXT, SEED)?;
    state.sync_all()?;

    write_private(run, MARKER, MARKER_CONTENT)?;
    run.sync_all()?;
    Ok(true)
}

fn run() -> io::Result<bool> {
    if std::env::args_os().len() != 1 {
        return Err(invalid("usage: ssh-seed (no arguments)"));
    }
    if unsafe { geteuid() } != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "must run as root"));
    }
    trusted_ancestors(Path::new(STATE))?;
    trusted_ancestors(Path::new(RUN))?;
    trusted_ancestors(Path::new("/dev"))?;
    let state = directory(Path::new(STATE), true)?;
    let run = directory(Path::new(RUN), false)?;
    let mut entropy = KernelEntropy::open()?;
    initialise(&state, &run, &mut entropy)
}

fn main() {
    match run() {
        Ok(true) => println!("ssh-seed: entropy initialised, seed rotated for the next boot"),
        Ok(false) => println!("ssh-seed: already initialised this boot"),
        Err(error) => {
            eprintln!("ssh-seed: unavailable: {error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(std::path::PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "dash-ssh-seed-test-{}-{}",
                std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn file(&self, name: &str, data: &[u8], mode: u32) {
            let path = self.0.join(name);
            fs::write(&path, data).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }
        fn open(&self) -> File {
            directory(&self.0, true).unwrap()
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[derive(Default)]
    struct FakeEntropy {
        credits: usize,
        reads: usize,
        fail_credit: bool,
        fail_read: bool,
    }
    impl Entropy for FakeEntropy {
        fn credit(&mut self, seed: &Seed) -> io::Result<()> {
            assert_eq!(seed.0, [0x31; SEED_LEN]);
            self.credits += 1;
            if self.fail_credit { return Err(invalid("simulated ioctl failure")); }
            Ok(())
        }
        fn fresh(&mut self) -> io::Result<Seed> {
            self.reads += 1;
            if self.fail_read { return Err(invalid("simulated random read failure")); }
            Ok(Seed([0x62; SEED_LEN]))
        }
    }

    fn setup() -> (TestDir, TestDir) {
        let state = TestDir::new();
        state.file("seed", &[0x31; SEED_LEN], 0o600);
        (state, TestDir::new())
    }

    #[test]
    fn payload_matches_linux_rand_pool_info() {
        let seed = Seed([0x31; SEED_LEN]);
        let pool = entropy_pool(&seed);
        assert_eq!(std::mem::size_of::<EntropyPool>(), 72);
        assert_eq!(pool.entropy_count, 512);
        assert_eq!(pool.buf_size, 64);
        assert_eq!(pool.data, seed.0);
        assert_eq!((&pool.data as *const _ as usize) - (&pool as *const _ as usize), 8);
        assert_eq!(RNDADDENTROPY, 0x4008_5203);
    }

    #[test]
    fn sensitive_arrays_can_be_erased() {
        let mut bytes = [0x31; SEED_LEN];
        erase(&mut bytes);
        assert_eq!(bytes, [0; SEED_LEN]);
    }

    #[test]
    fn ownership_and_modes_are_exact() {
        assert!(owner_mode(0, 0, 0o100600, 0o600).is_ok());
        for (uid, gid, mode) in [(1, 0, 0o600), (0, 1, 0o600), (0, 0, 0o640), (0, 0, 0o4600)] {
            assert!(owner_mode(uid, gid, mode, 0o600).is_err());
        }
    }

    #[test]
    fn state_directory_rejects_loose_permissions() {
        let dir = TestDir::new();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(directory(&dir.0, true).is_err());
    }

    #[test]
    fn runtime_directory_rejects_write_access_for_others() {
        let dir = TestDir::new();
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(directory(&dir.0, false).is_err());
    }

    #[test]
    fn directory_symlink_is_rejected() {
        let dir = TestDir::new();
        let other = TestDir::new();
        let link = other.0.join("link");
        symlink(&dir.0, &link).unwrap();
        assert!(directory(&link, true).is_err());
    }

    #[test]
    fn seed_symlink_is_rejected() {
        let dir = TestDir::new();
        dir.file("target", &[0x31; SEED_LEN], 0o600);
        symlink("target", dir.0.join("seed")).unwrap();
        assert!(read_seed(&dir.open()).is_err());
    }

    #[test]
    fn seed_hardlink_is_rejected() {
        let dir = TestDir::new();
        dir.file("target", &[0x31; SEED_LEN], 0o600);
        fs::hard_link(dir.0.join("target"), dir.0.join("seed")).unwrap();
        assert!(read_seed(&dir.open()).is_err());
    }

    #[test]
    fn seed_directory_is_rejected() {
        let dir = TestDir::new();
        fs::create_dir(dir.0.join("seed")).unwrap();
        assert!(read_seed(&dir.open()).is_err());
    }

    #[test]
    fn seed_fifo_is_rejected_without_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let dir = TestDir::new();
        let path = std::ffi::CString::new(dir.0.join("seed").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(read_seed(&dir.open()).is_err());
    }

    #[test]
    fn seed_length_is_exact() {
        let dir = TestDir::new();
        for length in [0, 63, 65, 128] {
            dir.file("seed", &vec![0x31; length], 0o600);
            assert!(read_seed(&dir.open()).is_err());
        }
    }

    #[test]
    fn seed_permissions_are_checked() {
        let dir = TestDir::new();
        dir.file("seed", &[0x31; SEED_LEN], 0o644);
        assert!(read_seed(&dir.open()).is_err());
    }

    #[test]
    fn valid_seed_is_read_exactly() {
        let (state, _run) = setup();
        assert_eq!(read_seed(&state.open()).unwrap().0, [0x31; SEED_LEN]);
    }

    #[test]
    fn marker_requires_exact_content_and_private_mode() {
        let dir = TestDir::new();
        for (contents, mode) in [(b"wrong!\n".as_slice(), 0o600), (b"seeded\n".as_slice(), 0o644)] {
            dir.file("dash-ssh-seeded", contents, mode);
            assert!(marker_present(&dir.open()).is_err());
        }
    }

    #[test]
    fn marker_symlink_is_rejected() {
        let dir = TestDir::new();
        dir.file("target", MARKER_CONTENT, 0o600);
        symlink("target", dir.0.join("dash-ssh-seeded")).unwrap();
        assert!(marker_present(&dir.open()).is_err());
    }

    #[test]
    fn exclusive_write_never_overwrites_a_file() {
        let dir = TestDir::new();
        dir.file("seed", b"existing", 0o600);
        assert!(write_private(&dir.open(), SEED, b"new").is_err());
        assert_eq!(fs::read(dir.0.join("seed")).unwrap(), b"existing");
    }

    #[test]
    fn exclusive_write_never_follows_a_symlink() {
        let dir = TestDir::new();
        dir.file("target", b"existing", 0o600);
        symlink("target", dir.0.join("seed")).unwrap();
        assert!(write_private(&dir.open(), SEED, b"new").is_err());
        assert_eq!(fs::read(dir.0.join("target")).unwrap(), b"existing");
        assert!(fs::symlink_metadata(dir.0.join("seed")).unwrap().file_type().is_symlink());
    }

    #[test]
    fn successful_initialisation_rotates_seed_then_marks_boot() {
        let (state, run) = setup();
        let mut entropy = FakeEntropy::default();
        assert!(initialise(&state.open(), &run.open(), &mut entropy).unwrap());
        assert_eq!(entropy.credits, 1);
        assert_eq!(entropy.reads, 1);
        assert_eq!(fs::read(state.0.join("seed")).unwrap(), [0x62; SEED_LEN]);
        private_file(&fs::metadata(state.0.join("seed")).unwrap(), 64).unwrap();
        assert!(marker_present(&run.open()).unwrap());
        assert!(!state.0.join(".seed-next").exists());
    }

    #[test]
    fn helper_restart_does_not_credit_again() {
        let (state, run) = setup();
        let mut entropy = FakeEntropy::default();
        initialise(&state.open(), &run.open(), &mut entropy).unwrap();
        assert!(!initialise(&state.open(), &run.open(), &mut entropy).unwrap());
        assert_eq!(entropy.credits, 1);
        assert_eq!(entropy.reads, 1);
    }

    #[test]
    fn invalid_marker_does_not_consume_seed() {
        let (state, run) = setup();
        run.file("dash-ssh-seeded", b"wrong!\n", 0o600);
        let mut entropy = FakeEntropy::default();
        assert!(initialise(&state.open(), &run.open(), &mut entropy).is_err());
        assert_eq!(entropy.credits, 0);
        assert!(state.0.join("seed").exists());
    }

    #[test]
    fn stale_rotation_file_fails_before_consuming_seed() {
        let (state, run) = setup();
        state.file(".seed-next", &[0x62; SEED_LEN], 0o600);
        let mut entropy = FakeEntropy::default();
        assert!(initialise(&state.open(), &run.open(), &mut entropy).is_err());
        assert_eq!(entropy.credits, 0);
        assert!(state.0.join("seed").exists());
    }

    #[test]
    fn stale_rotation_symlink_fails_before_consuming_seed() {
        let (state, run) = setup();
        symlink("missing", state.0.join(".seed-next")).unwrap();
        let mut entropy = FakeEntropy::default();
        assert!(initialise(&state.open(), &run.open(), &mut entropy).is_err());
        assert_eq!(entropy.credits, 0);
        assert!(state.0.join("seed").exists());
    }

    #[test]
    fn seed_is_removed_before_entropy_is_credited() {
        struct CheckRemoval(std::path::PathBuf);
        impl Entropy for CheckRemoval {
            fn credit(&mut self, _: &Seed) -> io::Result<()> {
                assert!(!self.0.join("seed").exists());
                Ok(())
            }
            fn fresh(&mut self) -> io::Result<Seed> {
                Ok(Seed([0x62; SEED_LEN]))
            }
        }
        let (state, run) = setup();
        initialise(&state.open(), &run.open(), &mut CheckRemoval(state.0.clone())).unwrap();
    }

    #[test]
    fn credit_failure_consumes_seed_without_marker() {
        let (state, run) = setup();
        let mut entropy = FakeEntropy { fail_credit: true, ..Default::default() };
        assert!(initialise(&state.open(), &run.open(), &mut entropy).is_err());
        assert!(!state.0.join("seed").exists());
        assert!(!marker_present(&run.open()).unwrap());
        assert_eq!(entropy.reads, 0);
        assert!(initialise(&state.open(), &run.open(), &mut entropy).is_err());
        assert_eq!(entropy.credits, 1);
    }

    #[test]
    fn random_read_failure_consumes_seed_without_marker() {
        let (state, run) = setup();
        let mut entropy = FakeEntropy { fail_read: true, ..Default::default() };
        assert!(initialise(&state.open(), &run.open(), &mut entropy).is_err());
        assert!(!state.0.join("seed").exists());
        assert!(!marker_present(&run.open()).unwrap());
    }

    #[test]
    fn missing_seed_does_not_attempt_entropy_operations() {
        let state = TestDir::new();
        let run = TestDir::new();
        let mut entropy = FakeEntropy::default();
        assert!(initialise(&state.open(), &run.open(), &mut entropy).is_err());
        assert_eq!(entropy.credits, 0);
        assert_eq!(entropy.reads, 0);
    }

    #[test]
    fn directory_lock_prevents_parallel_initialisation() {
        let dir = TestDir::new();
        let first = dir.open();
        let second = dir.open();
        lock(&first).unwrap();
        assert!(lock(&second).is_err());
        drop(first);
        lock(&second).unwrap();
    }
}
