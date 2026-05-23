//! Single-instance guard for the daemon. Writes the current PID to a file
//! atomically (`O_CREAT | O_EXCL`); if the file already exists, checks whether
//! that PID is still alive — alive → refuse to start; dead → take over the
//! stale lock. RAII: dropping the lock removes the file (best-effort; a kill
//! -9 leaves the file behind, and the next start detects it as stale).

use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use crate::error::{Result, RoyError};

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
            // Atomic create with owner-only mode — no window where the file
            // exists with permissive umask perms before we chmod it.
            let mut opts = OpenOptions::new();
            opts.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            match opts.open(&path) {
                Ok(mut file) => {
                    write_pid(&mut file, std::process::id())?;
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    let existing = read_pid(&path)?;
                    if let Some(pid) = existing {
                        if pid_alive(pid) {
                            return Err(RoyError::Protocol(format!(
                                "daemon already running (pid {pid}); pid file: {}",
                                path.display()
                            )));
                        }
                    }
                    // Stale lock — remove and retry. `remove_file` here is the
                    // only place that drops a pid file we did not create; it
                    // is safe because we just confirmed the owner is dead.
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

fn write_pid(file: &mut File, pid: u32) -> Result<()> {
    file.write_all(pid.to_string().as_bytes())
        .map_err(RoyError::Io)?;
    file.write_all(b"\n").map_err(RoyError::Io)?;
    file.sync_all().map_err(RoyError::Io)?;
    Ok(())
}

fn read_pid(path: &Path) -> Result<Option<i32>> {
    let mut s = String::new();
    File::open(path)
        .and_then(|mut f| f.read_to_string(&mut s))
        .map_err(RoyError::Io)?;
    Ok(s.trim().parse::<i32>().ok())
}

/// True if a process with `pid` is alive. Uses `kill(pid, 0)` which sends no
/// signal but performs the permission/existence checks: returns 0 iff a
/// process exists and we may signal it, errors with ESRCH iff no such pid.
fn pid_alive(pid: i32) -> bool {
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

    /// The pid file must be created with `0600` so a sibling user on a shared
    /// machine can't read which PID owns the daemon (and `kill` it with `kill
    /// -0` to probe presence). Atomic `OpenOptionsExt::mode` guarantees no
    /// race-window where the file exists under the umask default.
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
