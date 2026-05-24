//! Single-instance guard for the daemon. Writes the current PID to a file
//! atomically (`O_CREAT | O_EXCL`); if the file already exists, checks whether
//! that PID is still alive — alive → refuse to start; dead → take over the
//! stale lock. RAII: dropping the lock removes the file (best-effort; a kill
//! -9 leaves the file behind, and the next start detects it as stale).

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{Result, RoyError};

/// Atomically create `path` with `0600` and write `content` + `\n`, then
/// fsync. Errors with `ErrorKind::AlreadyExists` if the file is already there.
/// Used wherever the daemon owns a small ASCII control file (pid, token).
pub(crate) fn create_owner_only_file(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut opts = OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(content)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

pub struct PidLock {
    path: PathBuf,
}

impl PidLock {
    /// Acquire the lock at `path`. Errors with `RoyError::Protocol` if another
    /// live process owns it.
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(RoyError::Io)?;
        }
        loop {
            let pid_bytes = std::process::id().to_string();
            match create_owner_only_file(&path, pid_bytes.as_bytes()) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if let Some(pid) = read_pid(&path)? {
                        if pid_alive(pid) {
                            return Err(RoyError::Protocol(format!(
                                "daemon already running (pid {pid}); pid file: {}",
                                path.display()
                            )));
                        }
                    }
                    // Stale lock — remove and retry. Safe because the owner is
                    // confirmed dead; this is the only place we delete a pid
                    // file we did not create.
                    std::fs::remove_file(&path).map_err(RoyError::Io)?;
                }
                Err(e) => return Err(RoyError::Io(e)),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn read_pid(path: &Path) -> Result<Option<i32>> {
    let mut s = String::new();
    File::open(path)
        .and_then(|mut f| f.read_to_string(&mut s))
        .map_err(RoyError::Io)?;
    Ok(s.trim().parse::<i32>().ok())
}

/// Best-effort read of the pid stored at `path`. Returns `None` when the file
/// is missing, unreadable, or doesn't parse as an integer. Use this from
/// `status`-style commands that want to surface "who owns the lock right now?"
/// without trying to acquire it.
pub fn peek_pid(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Pid-file path the daemon installs alongside its Unix socket. Mirrors the
/// logic in `Daemon::run_with_opts`: `daemon.sock` → `daemon.sock.pid`, a
/// suffix-less socket → `<name>.pid`. Sharing this helper keeps the daemon
/// and `roy status` from drifting on the convention.
pub fn pid_path_for_socket(socket: &Path) -> PathBuf {
    socket.with_extension(
        socket
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!("{e}.pid"))
            .unwrap_or_else(|| "pid".to_string()),
    )
}

/// True if a process with `pid` is alive. Uses `kill(pid, 0)` which sends no
/// signal but performs the permission/existence checks: returns 0 iff a
/// process exists and we may signal it, errors with ESRCH iff no such pid.
pub fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_path() -> PathBuf {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!("roy-pidlock-test-{}-{n}.pid", std::process::id()))
    }

    #[test]
    fn double_acquire_on_same_path_fails_while_held() {
        let path = tmp_path();
        let lock = PidLock::acquire(&path).unwrap();
        match PidLock::acquire(&path) {
            Ok(_) => panic!("second acquire must fail while the first is held"),
            Err(RoyError::Protocol(msg)) => assert!(
                msg.contains("already running"),
                "unexpected error message: {msg}"
            ),
            Err(other) => panic!("unexpected error: {other:?}"),
        }
        drop(lock);
        // After drop the file is gone — and so a re-acquire works.
        let _ = PidLock::acquire(&path).unwrap();
    }

    #[test]
    fn stale_pid_file_is_taken_over() {
        let path = tmp_path();
        // Spawn-and-reap a real child so we have a definitively dead PID.
        let child = std::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");
        let dead_pid = child.id();
        let _ = std::process::Command::new("true").arg("--ignored").output(); // sync wait via a fresh call is enough for `true` to be reaped
                                                                              // Belt-and-braces: wait the real child too.
        let mut child = child;
        let _ = child.wait();

        // Write the dead PID into the lock path manually.
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(dead_pid.to_string().as_bytes()).unwrap();
        }

        // PidLock::acquire must detect the stale PID and take over.
        let _lock = PidLock::acquire(&path).unwrap();
    }

    /// Pid file must be created `0600` atomically — no race window where a
    /// sibling user could open it under the default umask before chmod.
    #[cfg(unix)]
    #[test]
    fn pid_file_is_created_with_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp_path();
        let _lock = PidLock::acquire(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "pid file must be 0600 regardless of umask, got {mode:o}"
        );
    }

    #[test]
    fn peek_pid_returns_value_when_file_holds_integer() {
        let path = tmp_path();
        std::fs::write(&path, "12345\n").unwrap();
        assert_eq!(peek_pid(&path), Some(12345));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn peek_pid_is_none_for_missing_or_garbage() {
        let path = tmp_path();
        assert_eq!(peek_pid(&path), None);
        std::fs::write(&path, "not-a-number\n").unwrap();
        assert_eq!(peek_pid(&path), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn pid_alive_true_for_own_pid_false_for_zero() {
        assert!(pid_alive(std::process::id() as i32));
        assert!(!pid_alive(0));
        assert!(!pid_alive(-1));
    }

    #[test]
    fn pid_path_for_socket_appends_pid_to_existing_extension() {
        let p = pid_path_for_socket(Path::new("/tmp/daemon.sock"));
        assert_eq!(p, PathBuf::from("/tmp/daemon.sock.pid"));
    }

    #[test]
    fn pid_path_for_socket_uses_pid_when_no_extension() {
        let p = pid_path_for_socket(Path::new("/tmp/daemon"));
        assert_eq!(p, PathBuf::from("/tmp/daemon.pid"));
    }

    #[test]
    fn live_pid_blocks_acquire() {
        let path = tmp_path();
        // Use OUR pid — definitely alive while the test runs.
        let live_pid = std::process::id();
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(live_pid.to_string().as_bytes()).unwrap();
        }
        match PidLock::acquire(&path) {
            Ok(_) => panic!("acquire must reject a file owned by a live pid"),
            Err(RoyError::Protocol(msg)) => assert!(msg.contains(&live_pid.to_string())),
            Err(other) => panic!("unexpected error: {other:?}"),
        }
        // Manual cleanup (we wrote the file by hand, no Drop fires).
        let _ = std::fs::remove_file(&path);
    }
}
