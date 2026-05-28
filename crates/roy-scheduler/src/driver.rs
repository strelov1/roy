//! Driver — the polling loop and per-fire invocation. Single-process
//! single-instance (PidLock added in Task 16).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use croner::Cron;
use sqlx::SqlitePool;

use crate::plan::plan_tick;
use crate::store::triggers;
use crate::types::Trigger;

/// A claimed trigger plus the `running` fire row already created for it.
/// `poll_tick` inserts the fire row inside the claim txn so the FK to
/// `triggers(id)` is still satisfied at INSERT time — without this,
/// oneshots (which `plan_tick` deletes in the same tick) would fail with
/// `FOREIGN KEY constraint failed` on the fire row.
#[derive(Debug, Clone)]
pub struct ClaimedFire {
    pub trigger: Trigger,
    pub fire_id: String,
}

/// One polling tick: claim due rows in a short transaction, return the
/// rows the caller should dispatch through invoke_fire (OUTSIDE the txn).
pub async fn poll_tick(pool: &SqlitePool, batch_limit: i64) -> Result<Vec<ClaimedFire>> {
    let now = Utc::now();
    let mut tx = pool.begin().await?;

    let due = triggers::select_due(&mut tx, now, batch_limit).await?;
    let plan = plan_tick(&due, now, compute_next);

    // Insert the `running` fire row for each to_fire trigger BEFORE the
    // trigger delete fires the ON DELETE SET NULL — otherwise a oneshot's
    // delete runs before the fire INSERT and SQLite rejects the INSERT.
    let mut claimed: Vec<ClaimedFire> = Vec::with_capacity(plan.to_fire.len());
    for trig in &plan.to_fire {
        let fire_id = fires::insert_running_in_txn(
            &mut tx,
            fires::NewFire {
                agent_id: trig.agent_id.clone(),
                trigger_id: Some(trig.id.clone()),
            },
        )
        .await?;
        claimed.push(ClaimedFire {
            trigger: trig.clone(),
            fire_id,
        });
    }

    for id in &plan.to_delete {
        triggers::delete_in_txn(&mut tx, id).await?;
    }
    for op in &plan.to_advance {
        triggers::advance_next_fire(&mut tx, &op.id, op.next_fire_at, op.last_fire_at).await?;
    }
    for op in &plan.to_pause {
        triggers::pause(&mut tx, &op.id, &op.last_error).await?;
    }

    tx.commit().await?;
    Ok(claimed)
}

/// croner-backed `next firing` function used by plan_tick.
fn compute_next(expr: &str, tz: &str) -> Option<DateTime<Utc>> {
    let cron = Cron::new(expr).parse().ok()?;
    let tz: chrono_tz::Tz = tz.parse().ok()?;
    let now = Utc::now().with_timezone(&tz);
    cron.find_next_occurrence(&now, false)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::roy_client::{self, FireOutcome};
use crate::store::{agents, fires};
use crate::subscribers;
use crate::types::{Agent, FireStatus};

#[derive(Debug, Clone)]
pub struct ServeOpts {
    pub db_path: PathBuf,
    pub socket_path: PathBuf,
    pub poll_interval: Duration,
    pub batch_limit: i64,
    pub max_fires: usize,
    pub fire_timeout: Duration,
}

impl Default for ServeOpts {
    fn default() -> Self {
        Self {
            db_path: crate::default_db_path(),
            socket_path: default_socket_path(),
            poll_interval: Duration::from_millis(1500),
            batch_limit: 50,
            max_fires: 8,
            fire_timeout: Duration::from_secs(600),
        }
    }
}

fn default_socket_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}

/// Top-level entry. Opens the DB, runs the crash-recovery sweep, then
/// polls forever. Caller installs the PidLock (see src/main.rs Task 18).
pub async fn serve(opts: ServeOpts) -> Result<()> {
    let pool = crate::db::open(&opts.db_path).await?;

    let swept = fires::sweep_running_older_than(&pool, fires::default_sweep_cutoff()).await?;
    if swept > 0 {
        tracing::warn!(rows = swept, "swept stuck running fires on startup");
    }

    let semaphore = Arc::new(tokio::sync::Semaphore::new(opts.max_fires));
    let pool = Arc::new(pool);
    let socket_path = Arc::new(opts.socket_path.clone());

    loop {
        match poll_tick(&pool, opts.batch_limit).await {
            Ok(to_fire) => {
                for ClaimedFire { trigger, fire_id } in to_fire {
                    let permit = match Arc::clone(&semaphore).acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => break,
                    };
                    let pool = Arc::clone(&pool);
                    let socket_path = Arc::clone(&socket_path);
                    let fire_timeout = opts.fire_timeout;
                    tokio::spawn(async move {
                        if let Err(e) =
                            invoke_fire(&pool, &socket_path, trigger, fire_id, fire_timeout).await
                        {
                            tracing::error!(error = %e, "invoke_fire failed");
                        }
                        drop(permit);
                    });
                }
            }
            Err(e) => tracing::error!(error = %e, "poll_tick failed"),
        }
        tokio::time::sleep(opts.poll_interval).await;
    }
}

/// Run a fire whose `running` row was already inserted upstream by
/// `poll_tick`. Fetches the agent, then delegates to `run_fire_for_agent`
/// — the shared post-insert pipeline used by both scheduled fires and
/// ad-hoc `fire-now` invocations.
pub async fn invoke_fire(
    pool: &SqlitePool,
    socket_path: &std::path::Path,
    trigger: Trigger,
    fire_id: String,
    fire_timeout: Duration,
) -> Result<()> {
    let agent = agents::get_by_id(pool, &trigger.agent_id)
        .await?
        .with_context(|| format!("agent {} missing", trigger.agent_id))?;

    let mut tags = BTreeMap::new();
    tags.insert("roy-scheduler:agent_id".into(), agent.id.clone());
    tags.insert("roy-scheduler:trigger_id".into(), trigger.id.clone());
    tags.insert("roy-scheduler:fire_id".into(), fire_id.clone());
    tags.insert("roy-scheduler:kind".into(), "background_fire".into());

    let _fire = run_fire_for_agent(pool, socket_path, &agent, fire_id, tags, fire_timeout).await?;
    Ok(())
}

/// Ad-hoc fire entry point — used by `roy-scheduler fire-now`. Inserts a
/// `running` fire row with `trigger_id = None`, then runs the same
/// post-insert pipeline as scheduled fires (terminal update, persistent
/// session capture, subscriber dispatch). Returns the terminal `Fire` row.
///
/// `initiated_by` records the session id of the caller that issued the fire
/// (e.g. a UI session, or another agent). When `Some`, the resulting fire
/// session carries the reserved tag `roy-scheduler:initiated_by_session` so
/// downstream consumers (UI, audit) can link a fire back to its initiator.
/// The legacy `roy-scheduler:parent_session_id` tag is reserved and not set
/// by the scheduler itself.
pub async fn fire_agent_ad_hoc(
    pool: &SqlitePool,
    socket_path: &std::path::Path,
    agent_id: &str,
    fire_timeout: Duration,
    initiated_by: Option<String>,
) -> Result<crate::types::Fire> {
    let agent = agents::get_by_id(pool, agent_id)
        .await?
        .with_context(|| format!("agent {agent_id} not found"))?;

    let fire_id = fires::insert_running(
        pool,
        fires::NewFire {
            agent_id: agent.id.clone(),
            trigger_id: None,
        },
    )
    .await?;

    let mut tags = BTreeMap::new();
    tags.insert("roy-scheduler:agent_id".into(), agent.id.clone());
    tags.insert("roy-scheduler:fire_id".into(), fire_id.clone());
    tags.insert("roy-scheduler:kind".into(), "fire_now".into());
    if let Some(parent) = initiated_by {
        tags.insert("roy-scheduler:initiated_by_session".into(), parent);
    }

    run_fire_for_agent(pool, socket_path, &agent, fire_id, tags, fire_timeout).await
}

/// Shared post-insert pipeline: invoke the daemon, write the terminal
/// fire row, capture the persistent session id if applicable, dispatch
/// subscribers. The `running` row must already exist at `fire_id`.
async fn run_fire_for_agent(
    pool: &SqlitePool,
    socket_path: &std::path::Path,
    agent: &Agent,
    fire_id: String,
    tags: BTreeMap<String, String>,
    fire_timeout: Duration,
) -> Result<crate::types::Fire> {
    let target = build_target(agent);
    let mut outcome = roy_client::fire(
        socket_path,
        target,
        effective_prompt(agent),
        tags.clone(),
        fire_timeout,
    )
    .await;

    // Spec §7: persistent_session_id points at a roy session that's gone
    // (daemon restart, eviction). Daemon returns NoSession. Clear the dead
    // id and retry once as a fresh Spawn so the agent isn't stuck forever.
    // We retry at most once; if the Spawn also fails we keep that outcome.
    let mut did_fallback_spawn = false;
    if let Ok(FireOutcome::Error { ref code, .. }) = outcome {
        if code == "no_session" && agent.is_persistent() && agent.persistent_session_id.is_some() {
            let old_sid = agent.persistent_session_id.clone().unwrap();
            tracing::warn!(
                agent_id = %agent.id,
                "persistent session {} gone — falling back to fresh spawn",
                old_sid,
            );
            agents::update_persistent_session_id(pool, &agent.id, None).await?;
            did_fallback_spawn = true;
            let retry_target = roy::FireTarget::Spawn {
                harness: agent.harness.clone(),
                system_prompt: None,
            };
            outcome = roy_client::fire(
                socket_path,
                retry_target,
                effective_prompt(agent),
                tags,
                fire_timeout,
            )
            .await;
        }
    }

    let (terminal, success_ref, error_msg) = match outcome {
        Ok(FireOutcome::Done(s)) => (
            fires::TerminalUpdate {
                status: FireStatus::Ok,
                session_id: Some(s.session_id.clone()),
                seq_range: Some((s.seq_range.0 as i64, s.seq_range.1 as i64)),
                assistant_text: Some(s.assistant_text.clone()),
                cost_usd: s.cost_usd,
                stop_reason: Some(s.stop_reason.clone()),
                error_message: None,
            },
            Some(s),
            None,
        ),
        Ok(FireOutcome::Timeout {
            session_id,
            partial_seq_range,
        }) => (
            fires::TerminalUpdate {
                status: FireStatus::Timeout,
                session_id: Some(session_id),
                seq_range: Some((partial_seq_range.0 as i64, partial_seq_range.1 as i64)),
                assistant_text: None,
                cost_usd: None,
                stop_reason: None,
                error_message: Some("fire timed out".into()),
            },
            None,
            Some("fire timed out".to_string()),
        ),
        Ok(FireOutcome::Error {
            session_id,
            code,
            message,
        }) => (
            fires::TerminalUpdate {
                status: FireStatus::Error,
                session_id,
                seq_range: None,
                assistant_text: None,
                cost_usd: None,
                stop_reason: None,
                error_message: Some(format!("{code}: {message}")),
            },
            None,
            Some(format!("{code}: {message}")),
        ),
        Err(e) => (
            fires::TerminalUpdate {
                status: FireStatus::Error,
                session_id: None,
                seq_range: None,
                assistant_text: None,
                cost_usd: None,
                stop_reason: None,
                error_message: Some(format!("roy_client: {e:#}")),
            },
            None,
            Some(format!("roy_client: {e:#}")),
        ),
    };

    fires::update_terminal(pool, &fire_id, terminal).await?;

    // If we used Spawn but the agent is persistent, capture the new session id.
    // `did_fallback_spawn` covers the retry case — the in-memory `agent` snapshot
    // still has the now-stale old id even though we cleared it in the DB above.
    if agent.is_persistent() && (agent.persistent_session_id.is_none() || did_fallback_spawn) {
        if let Some(ref s) = success_ref {
            agents::update_persistent_session_id(pool, &agent.id, Some(&s.session_id)).await?;
        }
    }

    let fire = fires::get_by_id(pool, &fire_id).await?.expect("fire row");
    subscribers::dispatch(
        pool,
        socket_path,
        &fire,
        &agent.name,
        success_ref.as_ref(),
        error_msg.as_deref(),
    )
    .await?;

    Ok(fire)
}

/// The prompt sent to the agent on a fire. When the agent has a
/// `notify_session`, append a single-line marker carrying the parent session
/// id. Operational guidance (when/how to call `roy inject`) lives in the
/// `roy-inject` skill loaded by the agent — its `description` matches this
/// marker, so the agent learns what to do from its skill, not from a long
/// instruction baked into every prompt.
fn effective_prompt(agent: &Agent) -> String {
    match &agent.notify_session {
        None => agent.task.clone(),
        Some(sid) => format!("{}\n\n[roy-bg] notify_session={sid}", agent.task),
    }
}

fn build_target(agent: &Agent) -> roy::FireTarget {
    if agent.is_persistent() {
        if let Some(sid) = agent.persistent_session_id.as_ref() {
            return roy::FireTarget::Resume {
                session_id: sid.clone(),
            };
        }
    }
    roy::FireTarget::Spawn {
        harness: agent.harness.clone(),
        system_prompt: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db,
        store::{agents, triggers as tstore},
    };
    use chrono::Duration as CDur;
    use tempfile::tempdir;

    fn agent_with(task: &str, notify_session: Option<&str>) -> Agent {
        Agent {
            id: "agent-id".into(),
            name: "n".into(),
            harness: "claude".into(),
            project_id: None,
            task: task.into(),
            model: None,
            persistent: 0,
            persistent_session_id: None,
            notify_session: notify_session.map(str::to_string),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn effective_prompt_appends_notify_marker_when_set() {
        // The marker is what the agent-side `roy-inject` skill keys off of;
        // keep it stable across refactors.
        let sid = "11111111-1111-4111-8111-111111111111";
        let agent = agent_with("t", Some(sid));
        let p = effective_prompt(&agent);
        assert!(p.starts_with("t"), "preserves task at the start");
        assert!(
            p.contains(&format!("[roy-bg] notify_session={sid}")),
            "carries the marker so the agent's skill can pick the id up: {p:?}",
        );
    }

    #[test]
    fn effective_prompt_is_task_when_notify_unset() {
        let agent = agent_with("just the task", None);
        assert_eq!(effective_prompt(&agent), "just the task");
    }

    #[tokio::test]
    async fn poll_tick_advances_cron_and_returns_to_fire() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                harness: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: false,
                notify_session: None,
            },
        )
        .await
        .unwrap();
        let _trig = tstore::insert_cron(
            &pool,
            tstore::NewCronTrigger {
                agent_id: a.id,
                cron_expr: "*/5 * * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now() - CDur::seconds(10),
            },
        )
        .await
        .unwrap();

        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert_eq!(to_fire.len(), 1);

        // Second tick: nothing (next_fire_at was advanced).
        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert!(to_fire.is_empty());
    }

    #[tokio::test]
    async fn poll_tick_pauses_bad_cron() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                harness: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: false,
                notify_session: None,
            },
        )
        .await
        .unwrap();
        let t = tstore::insert_cron(
            &pool,
            tstore::NewCronTrigger {
                agent_id: a.id,
                cron_expr: "this-is-garbage".into(),
                timezone: "UTC".into(),
                next_fire_at: Utc::now() - CDur::seconds(10),
            },
        )
        .await
        .unwrap();

        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert!(to_fire.is_empty());

        let back = tstore::get_by_id(&pool, &t.id).await.unwrap().unwrap();
        assert!(back.is_paused());
        assert_eq!(back.last_error.as_deref(), Some("invalid cron"));
    }

    /// Spawn a mock roy daemon at `path` that replies to one ClientCommand
    /// with the given ServerEvent. Mirrors roy_client::tests::spawn_mock —
    /// kept inline (rather than exported) so the test stays self-contained.
    async fn spawn_mock_daemon(path: std::path::PathBuf, reply: roy::ServerEvent) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixListener;
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = sock.into_split();
            let mut lines = BufReader::new(rd).lines();
            let _cmd_line = lines.next_line().await.unwrap();
            let out = serde_json::to_string(&reply).unwrap();
            wr.write_all(out.as_bytes()).await.unwrap();
            wr.write_all(b"\n").await.unwrap();
        });
    }

    /// Spawn a mock roy daemon that replies with `first` on the first
    /// connection and `second` on the second. Used to drive the NoSession
    /// fallback retry path.
    async fn spawn_mock_daemon_seq(
        path: std::path::PathBuf,
        first: roy::ServerEvent,
        second: roy::ServerEvent,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixListener;
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            for reply in [first, second] {
                let (sock, _) = listener.accept().await.unwrap();
                let (rd, mut wr) = sock.into_split();
                let mut lines = BufReader::new(rd).lines();
                let _cmd_line = lines.next_line().await.unwrap();
                let out = serde_json::to_string(&reply).unwrap();
                wr.write_all(out.as_bytes()).await.unwrap();
                wr.write_all(b"\n").await.unwrap();
            }
        });
    }

    /// Spawn a mock daemon that captures the raw JSON line sent by the
    /// client into the shared `Mutex<Vec<String>>`, then replies with the
    /// given event. Used to assert on the wire-level tag map.
    async fn spawn_mock_daemon_capturing(
        path: std::path::PathBuf,
        reply: roy::ServerEvent,
        captured: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixListener;
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = sock.into_split();
            let mut lines = BufReader::new(rd).lines();
            let cmd_line = lines.next_line().await.unwrap().unwrap_or_default();
            captured.lock().unwrap().push(cmd_line);
            let out = serde_json::to_string(&reply).unwrap();
            wr.write_all(out.as_bytes()).await.unwrap();
            wr.write_all(b"\n").await.unwrap();
        });
    }

    #[tokio::test]
    async fn fire_agent_ad_hoc_dispatches_subscribers() {
        use crate::store::subscribers as substore;
        use crate::types::SubscriberKind;
        use roy::{ServerEvent, StopReason, TurnEvent};
        use wiremock::matchers::{method, path as wpath};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // 1. Webhook target.
        let webhook = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wpath("/hook"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ack"))
            .mount(&webhook)
            .await;

        // 2. Mock roy daemon at a tempdir UDS path.
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("roy.sock");
        spawn_mock_daemon(
            sock_path.clone(),
            ServerEvent::FireDone {
                session: "sid-ad-hoc".into(),
                seq_range: (1, 4),
                result: TurnEvent::Result {
                    cost_usd: Some(0.02),
                    stop_reason: StopReason::EndTurn,
                },
                assistant_text: "ad-hoc body".into(),
            },
        )
        .await;

        // 3. DB with an agent + an agent-scope webhook subscriber.
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "ad-hoc-agent".into(),
                harness: "claude".into(),
                project_id: None,
                task: "task".into(),
                model: None,
                persistent: false,
                notify_session: None,
            },
        )
        .await
        .unwrap();
        let cfg = format!(
            r#"{{"url":"{}/hook","body_template":"text={{{{result.assistant_text}}}}"}}"#,
            webhook.uri()
        );
        substore::insert(
            &pool,
            substore::NewSubscriber {
                agent_id: Some(a.id.clone()),
                trigger_id: None,
                kind: SubscriberKind::Webhook,
                config_json: cfg,
                order_index: 0,
            },
        )
        .await
        .unwrap();

        // 4. Fire ad-hoc. Subscribers should run.
        let fire = fire_agent_ad_hoc(&pool, &sock_path, &a.id, Duration::from_secs(5), None)
            .await
            .unwrap();
        assert_eq!(fire.status, "ok");
        assert_eq!(fire.session_id.as_deref(), Some("sid-ad-hoc"));

        // 5. The webhook should have received the rendered body.
        let reqs = webhook.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(String::from_utf8_lossy(&reqs[0].body), "text=ad-hoc body");

        // 6. A fire_subscriber_runs row should exist (ok).
        let runs = substore::list_runs_for_fire(&pool, &fire.id).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "ok");
    }

    #[tokio::test]
    async fn fire_agent_ad_hoc_captures_persistent_session_id() {
        use roy::{ServerEvent, StopReason, TurnEvent};

        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("roy.sock");
        spawn_mock_daemon(
            sock_path.clone(),
            ServerEvent::FireDone {
                session: "captured-sid".into(),
                seq_range: (1, 2),
                result: TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: StopReason::EndTurn,
                },
                assistant_text: "ok".into(),
            },
        )
        .await;

        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "persist".into(),
                harness: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: true,
                notify_session: None,
            },
        )
        .await
        .unwrap();
        assert!(a.persistent_session_id.is_none());

        fire_agent_ad_hoc(&pool, &sock_path, &a.id, Duration::from_secs(5), None)
            .await
            .unwrap();

        let back = agents::get_by_id(&pool, &a.id).await.unwrap().unwrap();
        assert_eq!(back.persistent_session_id.as_deref(), Some("captured-sid"));
    }

    #[tokio::test]
    async fn fire_agent_ad_hoc_records_initiated_by_when_parent_provided() {
        use roy::{ClientCommand, ServerEvent, StopReason, TurnEvent};

        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("roy.sock");
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        spawn_mock_daemon_capturing(
            sock_path.clone(),
            ServerEvent::FireDone {
                session: "sid-with-parent".into(),
                seq_range: (1, 2),
                result: TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: StopReason::EndTurn,
                },
                assistant_text: "ok".into(),
            },
            captured.clone(),
        )
        .await;

        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "with-parent".into(),
                harness: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: false,
                notify_session: None,
            },
        )
        .await
        .unwrap();

        let fire = fire_agent_ad_hoc(
            &pool,
            &sock_path,
            &a.id,
            Duration::from_secs(5),
            Some("parent-sid".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(fire.status, "ok");

        // The Fire command that hit the daemon must carry the
        // initiated_by_session tag and must NOT carry the reserved
        // parent_session_id tag (no scheduler component sets it any more).
        let lines = captured.lock().unwrap().clone();
        assert_eq!(lines.len(), 1, "expected exactly one Fire command");
        let cmd: ClientCommand = serde_json::from_str(&lines[0]).expect("parse ClientCommand");
        let ClientCommand::Fire { tags, .. } = cmd else {
            panic!("expected ClientCommand::Fire, got {cmd:?}");
        };
        assert_eq!(
            tags.get("roy-scheduler:initiated_by_session")
                .map(String::as_str),
            Some("parent-sid"),
        );
        assert!(
            !tags.contains_key("roy-scheduler:parent_session_id"),
            "fire-now must not set the reserved parent_session_id tag",
        );
    }

    #[tokio::test]
    async fn persistent_fire_retries_as_spawn_when_session_is_gone() {
        use roy::{ErrorCode, ServerEvent, StopReason, TurnEvent};

        // Daemon replies NoSession on the first connection (the Resume
        // attempt) and FireDone with a fresh session id on the second
        // (the Spawn fallback). The scheduler should:
        //   1. clear the dead persistent_session_id,
        //   2. re-fire as Spawn,
        //   3. capture the new id,
        //   4. write a terminal status='ok' fire row.
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("roy.sock");
        spawn_mock_daemon_seq(
            sock_path.clone(),
            ServerEvent::FireError {
                session: None,
                code: ErrorCode::NoSession,
                message: "session not found".into(),
            },
            ServerEvent::FireDone {
                session: "fresh-sid".into(),
                seq_range: (1, 3),
                result: TurnEvent::Result {
                    cost_usd: Some(0.01),
                    stop_reason: StopReason::EndTurn,
                },
                assistant_text: "after spawn".into(),
            },
        )
        .await;

        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "persist".into(),
                harness: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: true,
                notify_session: None,
            },
        )
        .await
        .unwrap();
        // Seed a dead persistent_session_id (as if a previous daemon spawned it).
        agents::update_persistent_session_id(&pool, &a.id, Some("dead-sid"))
            .await
            .unwrap();

        let fire = fire_agent_ad_hoc(&pool, &sock_path, &a.id, Duration::from_secs(5), None)
            .await
            .unwrap();

        // Terminal status reflects the retry's outcome, not the initial NoSession.
        assert_eq!(fire.status, "ok");
        assert_eq!(fire.session_id.as_deref(), Some("fresh-sid"));

        // The agent's persistent_session_id was rewritten to the new id.
        let back = agents::get_by_id(&pool, &a.id).await.unwrap().unwrap();
        assert_eq!(back.persistent_session_id.as_deref(), Some("fresh-sid"));
    }

    #[tokio::test]
    async fn poll_tick_deletes_oneshot_and_returns_it() {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("t.db")).await.unwrap();
        let a = agents::insert(
            &pool,
            agents::NewAgent {
                name: "x".into(),
                harness: "claude".into(),
                project_id: None,
                task: "t".into(),
                model: None,
                persistent: false,
                notify_session: None,
            },
        )
        .await
        .unwrap();
        let agent_id = a.id.clone();
        let t = tstore::insert_oneshot(
            &pool,
            tstore::NewOneshotTrigger {
                agent_id: a.id,
                fire_at: Utc::now() - CDur::seconds(10),
            },
        )
        .await
        .unwrap();

        let to_fire = poll_tick(&pool, 50).await.unwrap();
        assert_eq!(to_fire.len(), 1);
        assert_eq!(to_fire[0].trigger.id, t.id);

        // Trigger is gone after the tick.
        assert!(tstore::get_by_id(&pool, &t.id).await.unwrap().is_none());

        // The fire row exists and its trigger_id was set NULL by the
        // ON DELETE SET NULL fk (the oneshot trigger was deleted in the
        // same claim txn that inserted the fire).
        let fire = fires::get_by_id(&pool, &to_fire[0].fire_id)
            .await
            .unwrap()
            .expect("fire row");
        assert_eq!(fire.status, "running");
        assert_eq!(fire.agent_id, agent_id);
        assert!(fire.trigger_id.is_none());
    }
}
