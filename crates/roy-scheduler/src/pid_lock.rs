//! Single-instance guard for `roy-scheduler serve`. Mirrors the pattern in
//! `crates/roy/src/pid_lock.rs` — kept as a sibling instead of `pub use`d
//! across crates because the boundary rule (lib.rs doc) says roy-scheduler
//! imports only the control protocol from roy.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

/// Atomically create `path` with `0600` and write `content` + `\n`, then
/// fsync. Errors with `ErrorKind::AlreadyExists` if the file is already there.
fn create_owner_only_file(path: &Path, content: &[u8]) -> io::Result<()> {
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

#[derive(Debug)]
pub struct PidLock {
    path: PathBuf,
}

impl PidLock {
    /// Acquire the lock at `path`. Errors if another live process owns it.
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all {}", parent.display()))?;
        }
        loop {
            let pid_bytes = std::process::id().to_string();
            match create_owner_only_file(&path, pid_bytes.as_bytes()) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    if let Some(pid) = read_pid(&path)? {
                        if pid_alive(pid) {
                            return Err(anyhow!(
                                "roy-scheduler already running (pid {pid}); pid file: {}",
                                path.display()
                            ));
                        }
                    }
                    // Stale lock — remove and retry. Safe because the owner is
                    // confirmed dead; this is the only place we delete a pid
                    // file we did not create.
                    std::fs::remove_file(&path)
                        .with_context(|| format!("remove stale pid file {}", path.display()))?;
                }
                Err(e) => return Err(e).context("create pid file"),
            }
        }
    }

    #[allow(dead_code)]
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
        .with_context(|| format!("read pid file {}", path.display()))?;
    Ok(s.trim().parse::<i32>().ok())
}

/// True if a process with `pid` is alive. Uses `kill(pid, 0)` which sends no
/// signal but performs the permission/existence checks: returns 0 iff a
/// process exists and we may signal it, errors with ESRCH iff no such pid.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // Safety: kill(2) with sig=0 is a pure existence/permission probe.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32) -> bool {
    // Best-effort on non-Unix: refuse to take over.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmp_path() -> PathBuf {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::env::temp_dir().join(format!(
            "roy-scheduler-pidlock-test-{}-{n}.pid",
            std::process::id()
        ))
    }

    #[test]
    fn double_acquire_on_same_path_fails_while_held() {
        let path = tmp_path();
        let lock = PidLock::acquire(&path).unwrap();
        let err = PidLock::acquire(&path).expect_err("second acquire must fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("already running"), "unexpected error: {msg}");
        drop(lock);
        let _ = PidLock::acquire(&path).unwrap();
    }

    #[test]
    fn stale_pid_file_is_taken_over() {
        let path = tmp_path();
        // Spawn-and-reap a real child so we have a definitively dead PID.
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn `true`");
        let dead_pid = child.id();
        let _ = child.wait();

        // Write the dead PID into the lock path manually.
        {
            let mut f = std::fs::File::create(&path).unwrap();
            f.write_all(dead_pid.to_string().as_bytes()).unwrap();
        }

        let _lock = PidLock::acquire(&path).unwrap();
    }
}
