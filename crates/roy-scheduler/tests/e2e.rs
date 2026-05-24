//! End-to-end: real roy daemon + real roy-scheduler + real preset binary.
//!
//! Ignored by default — runs only when both binaries are built and
//! `cargo test -p roy-scheduler --test e2e -- --ignored` is requested
//! explicitly. Even then, the test self-skips when `opencode` is not on
//! `PATH`, so CI can run `--ignored` without the dependency installed.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use tempfile::tempdir;

struct DropChild(Child);
impl Drop for DropChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `CARGO_BIN_EXE_<name>` is only set for binaries in the *same* package
/// as the test. The `roy` binary lives in `roy-cli`, so resolve it by
/// taking the scheduler binary's parent directory and looking for `roy`
/// alongside it (Cargo puts every workspace binary in the same target dir).
fn sibling_bin(name: &str) -> PathBuf {
    let me = PathBuf::from(env!("CARGO_BIN_EXE_roy-scheduler"));
    let dir = me.parent().expect("binary has a parent dir");
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    dir.join(exe)
}

#[test]
#[ignore]
fn e2e_fire_completes() {
    // Self-skip when the real opencode binary isn't on PATH. The test still
    // counts as passing — running `--ignored` without the dependency
    // installed shouldn't fail CI.
    if which::which("opencode").is_err() {
        eprintln!("e2e_fire_completes: `opencode` not on PATH — skipping");
        return;
    }

    let roy_bin = sibling_bin("roy");
    let scheduler_bin = sibling_bin("roy-scheduler");
    assert!(roy_bin.exists(), "roy binary not built at {roy_bin:?}");
    assert!(
        scheduler_bin.exists(),
        "roy-scheduler binary not built at {scheduler_bin:?}"
    );

    let dir = tempdir().unwrap();
    let socket = dir.path().join("roy.sock");
    let db = dir.path().join("scheduler.db");
    let journal = dir.path().join("journals");
    let workspace = dir.path().join("workspace");
    let sched_pid = dir.path().join("scheduler.pid");
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();

    // 1. Start roy daemon.
    let _roy = DropChild(
        Command::new(&roy_bin)
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--journal-dir")
            .arg(&journal)
            .arg("--workspace-dir")
            .arg(&workspace)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("roy daemon"),
    );
    // Wait for the socket to appear (up to ~3s).
    let socket_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !socket.exists() && std::time::Instant::now() < socket_deadline {
        sleep(Duration::from_millis(50));
    }
    assert!(
        socket.exists(),
        "roy daemon never created socket {socket:?}"
    );

    // 2. roy-scheduler: migrate the DB, then register agent + oneshot trigger.
    let scheduler = scheduler_bin.to_str().unwrap();
    let env_pairs = [
        ("ROY_SCHEDULER_DB", db.display().to_string()),
        ("ROY_SOCKET", socket.display().to_string()),
    ];

    run(scheduler, &env_pairs, &["migrate"]);
    let agent_out = run_capture(
        scheduler,
        &env_pairs,
        &[
            "agents",
            "add",
            "--name",
            "smoke",
            "--preset",
            "opencode",
            "--task",
            "say something",
        ],
    );
    // Parse agent id from the printed JSON line.
    let v: serde_json::Value = serde_json::from_str(agent_out.trim())
        .unwrap_or_else(|e| panic!("agents add output not JSON ({e}):\n{agent_out}"));
    let agent_id = v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("agents add JSON missing string `id`:\n{agent_out}"))
        .to_string();

    let in_one_sec = chrono::Utc::now() + chrono::Duration::seconds(1);
    run(
        scheduler,
        &env_pairs,
        &[
            "triggers",
            "add",
            "--agent",
            &agent_id,
            "--oneshot",
            &in_one_sec.to_rfc3339(),
        ],
    );

    // 3. Start roy-scheduler serve.
    let _sched = DropChild(
        Command::new(scheduler)
            .env("ROY_SCHEDULER_DB", db.display().to_string())
            .env("ROY_SOCKET", socket.display().to_string())
            .arg("serve")
            .arg("--pid-file")
            .arg(&sched_pid)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("roy-scheduler serve"),
    );

    // 4. Poll fires list until we see one finished.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut last = String::new();
    while std::time::Instant::now() < deadline {
        let out = run_capture(
            scheduler,
            &env_pairs,
            &["fires", "list", "--agent", &agent_id, "--limit", "5"],
        );
        last = out.clone();
        if out.contains("\"status\":\"ok\"") {
            return; // success
        }
        sleep(Duration::from_millis(500));
    }
    panic!("fire never completed; last fires-list output:\n{last}");
}

fn run(bin: &str, env: &[(&str, String)], args: &[&str]) {
    let mut cmd = Command::new(bin);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let st = cmd.args(args).status().unwrap();
    assert!(st.success(), "{} {:?} exited {st}", bin, args);
}

fn run_capture(bin: &str, env: &[(&str, String)], args: &[&str]) -> String {
    let mut cmd = Command::new(bin);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{} {:?} failed:\n{}",
        bin,
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
