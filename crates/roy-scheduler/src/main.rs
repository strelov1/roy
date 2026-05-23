use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use roy::control::{ClientCommand, FireTarget, ServerEvent};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Pool, Row, Sqlite};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use uuid::Uuid;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long, env = "ROY_SCHEDULER_DB")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Schedule a new background task.
    Schedule {
        #[arg(short, long)]
        agent: Option<String>,
        #[arg(short, long)]
        session: Option<String>,
        #[arg(short, long)]
        prompt: String,
        #[arg(short, long)]
        cron: Option<String>,
        /// Tags in key=value format.
        #[arg(short, long)]
        tag: Vec<String>,
    },
    /// List all scheduled tasks.
    List,
    /// Cancel a scheduled task.
    Cancel { id: String },
    /// Run the scheduler daemon.
    Serve {
        #[arg(short, long, env = "ROY_SOCKET")]
        socket: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let db_path = cli.db.unwrap_or_else(|| PathBuf::from("roy-scheduler.db"));
    let db_url = format!("sqlite://{}", db_path.display());

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
        .context("Failed to connect to SQLite")?;

    // Initial migration.
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY,
            target_type TEXT NOT NULL,
            target_id TEXT NOT NULL,
            prompt TEXT NOT NULL,
            tags TEXT NOT NULL,
            schedule_cron TEXT,
            next_run_at DATETIME NOT NULL,
            status TEXT NOT NULL,
            last_run_at DATETIME,
            last_result TEXT,
            created_at DATETIME NOT NULL
        )",
    )
    .execute(&pool)
    .await?;

    match cli.command {
        Commands::Schedule {
            agent,
            session,
            prompt,
            cron,
            tag,
        } => {
            cmd_schedule(pool, agent, session, prompt, cron, tag).await?;
        }
        Commands::List => {
            cmd_list(pool).await?;
        }
        Commands::Cancel { id } => {
            cmd_cancel(pool, id).await?;
        }
        Commands::Serve { socket } => {
            let socket_path = socket.unwrap_or_else(|| {
                std::env::var_os("ROY_SOCKET")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/tmp/roy.sock"))
            });
            cmd_serve(pool, socket_path).await?;
        }
    }

    Ok(())
}

async fn cmd_schedule(
    pool: Pool<Sqlite>,
    agent: Option<String>,
    session: Option<String>,
    prompt: String,
    cron_str: Option<String>,
    tags_raw: Vec<String>,
) -> anyhow::Result<()> {
    let (target_type, target_id) = match (agent, session) {
        (Some(a), None) => ("spawn", a),
        (None, Some(s)) => ("resume", s),
        _ => anyhow::bail!("Specify either --agent or --session"),
    };

    let mut tags = BTreeMap::new();
    for t in tags_raw {
        if let Some((k, v)) = t.split_once('=') {
            tags.insert(k.to_string(), v.to_string());
        }
    }
    let tags_json = serde_json::to_string(&tags)?;

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let mut next_run_at = now;

    if let Some(ref c) = cron_str {
        let schedule = cron::Schedule::from_str(c).map_err(|e| anyhow!("invalid cron: {e}"))?;
        next_run_at = schedule
            .after(&now)
            .next()
            .ok_or_else(|| anyhow!("no next run for cron"))?;
    }

    sqlx::query(
        "INSERT INTO tasks (id, target_type, target_id, prompt, tags, schedule_cron, next_run_at, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(target_type)
    .bind(target_id)
    .bind(prompt)
    .bind(tags_json)
    .bind(cron_str)
    .bind(next_run_at)
    .bind("pending")
    .bind(now)
    .execute(&pool)
    .await?;

    println!("Task scheduled: {id}, next run: {next_run_at}");
    Ok(())
}

async fn cmd_list(pool: Pool<Sqlite>) -> anyhow::Result<()> {
    let rows = sqlx::query("SELECT id, target_type, target_id, status, next_run_at FROM tasks ORDER BY created_at DESC")
        .fetch_all(&pool)
        .await?;

    for row in rows {
        let id: String = row.get("id");
        let target_type: String = row.get("target_type");
        let target_id: String = row.get("target_id");
        let status: String = row.get("status");
        let next_run_at: DateTime<Utc> = row.get("next_run_at");
        println!("{id} | {target_type}:{target_id} | {status} | next: {next_run_at}");
    }
    Ok(())
}

async fn cmd_cancel(pool: Pool<Sqlite>, id: String) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM tasks WHERE id = ?")
        .bind(id)
        .execute(&pool)
        .await?;
    println!("Task cancelled.");
    Ok(())
}

async fn cmd_serve(pool: Pool<Sqlite>, socket_path: PathBuf) -> anyhow::Result<()> {
    tracing::info!(?socket_path, "scheduler daemon starting");

    loop {
        if let Err(e) = poll_tick(&pool, &socket_path).await {
            tracing::error!(error = %e, "poll_tick failed");
        }
        if let Err(e) = plan_tick(&pool).await {
            tracing::error!(error = %e, "plan_tick failed");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn poll_tick(pool: &Pool<Sqlite>, socket_path: &Path) -> anyhow::Result<()> {
    let now = Utc::now();
    let pending_tasks = sqlx::query(
        "SELECT id, target_type, target_id, prompt, tags FROM tasks
         WHERE status = 'pending' AND next_run_at <= ?",
    )
    .bind(now)
    .fetch_all(pool)
    .await?;

    for row in pending_tasks {
        let task_id: String = row.get("id");
        let target_type: String = row.get("target_type");
        let target_id: String = row.get("target_id");
        let prompt: String = row.get("prompt");
        let tags_json: String = row.get("tags");

        tracing::info!(%task_id, "firing task");

        // Mark as running.
        sqlx::query("UPDATE tasks SET status = 'running' WHERE id = ?")
            .bind(&task_id)
            .execute(pool)
            .await?;

        let pool = pool.clone();
        let socket_path = socket_path.to_path_buf();
        let target = match target_type.as_str() {
            "spawn" => FireTarget::Spawn {
                preset: target_id,
                cwd: None,
            },
            "resume" => FireTarget::Resume {
                session_id: target_id,
            },
            _ => {
                tracing::error!(%task_id, "unknown target type: {}", target_type);
                continue;
            }
        };
        let tags: BTreeMap<String, String> = serde_json::from_str(&tags_json).unwrap_or_default();

        tokio::spawn(async move {
            match execute_fire(&socket_path, target, prompt, tags.clone()).await {
                Ok((session_id, result_json)) => {
                    tracing::info!(%task_id, %session_id, "task completed");
                    let _ = sqlx::query(
                        "UPDATE tasks SET status = 'completed', last_run_at = ?, last_result = ? WHERE id = ?",
                    )
                    .bind(Utc::now())
                    .bind(&result_json)
                    .bind(&task_id)
                    .execute(&pool)
                    .await;

                    let _ = notify_subscribers(&task_id, &result_json, true, &tags).await;
                }
                Err(e) => {
                    tracing::error!(%task_id, error = %e, "task failed");
                    let _ = sqlx::query(
                        "UPDATE tasks SET status = 'failed', last_run_at = ?, last_result = ? WHERE id = ?",
                    )
                    .bind(Utc::now())
                    .bind(e.to_string())
                    .bind(&task_id)
                    .execute(&pool)
                    .await;

                    let _ = notify_subscribers(&task_id, &e.to_string(), false, &tags).await;
                }
            }
        });
    }

    Ok(())
}

async fn plan_tick(pool: &Pool<Sqlite>) -> anyhow::Result<()> {
    let tasks = sqlx::query("SELECT id, schedule_cron FROM tasks WHERE schedule_cron IS NOT NULL AND status IN ('completed', 'failed')")
        .fetch_all(pool)
        .await?;

    for row in tasks {
        let id: String = row.get("id");
        let cron_str: String = row.get("schedule_cron");

        if let Ok(schedule) = cron::Schedule::from_str(&cron_str) {
            if let Some(next) = schedule.after(&Utc::now()).next() {
                tracing::info!(%id, %next, "rescheduling recurring task");
                sqlx::query("UPDATE tasks SET status = 'pending', next_run_at = ? WHERE id = ?")
                    .bind(next)
                    .bind(id)
                    .execute(pool)
                    .await?;
            }
        }
    }
    Ok(())
}

async fn execute_fire(
    socket_path: &Path,
    target: FireTarget,
    prompt: String,
    tags: BTreeMap<String, String>,
) -> anyhow::Result<(String, String)> {
    let stream = UnixStream::connect(socket_path)
        .await
        .context("connecting to roy daemon")?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let cmd = ClientCommand::Fire {
        target,
        prompt,
        tags,
        timeout_ms: Some(600_000),
    };

    let line = serde_json::to_string(&cmd)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    loop {
        let line = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon hung up"))?;
        let event: ServerEvent = serde_json::from_str(&line)?;

        match event {
            ServerEvent::FireDone { session, .. } => {
                return Ok((session, line));
            }
            ServerEvent::FireTimeout { session, .. } => {
                return Err(anyhow!("fire timeout for session {session}"));
            }
            ServerEvent::FireError {
                session,
                code,
                message,
            } => {
                return Err(anyhow!(
                    "fire error (session={session:?}): {code:?}: {message}"
                ));
            }
            ServerEvent::Error { code, message, .. } => {
                return Err(anyhow!("daemon error: {code:?}: {message}"));
            }
            _ => {}
        }
    }
}

async fn notify_subscribers(
    task_id: &str,
    result: &str,
    success: bool,
    tags: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    if let Some(url) = tags.get("roy-scheduler:webhook_url") {
        let client = reqwest::Client::new();
        match client
            .post(url)
            .header("Content-Type", "application/json")
            .body(result.to_string())
            .send()
            .await
        {
            Ok(_) => tracing::info!(%task_id, %url, "webhook notification sent"),
            Err(e) => tracing::error!(%task_id, %url, error = %e, "webhook notification failed"),
        }
    }
    tracing::info!(%task_id, %success, "notification processing done");
    Ok(())
}
