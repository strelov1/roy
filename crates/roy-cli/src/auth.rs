//! `roy auth login | whoami | reset` — interactive CLI for the HTTP API + DB.
//!
//! `login` and `whoami` talk to the `roy-management` HTTP service. The cookie
//! returned by `POST /auth/login` is persisted under
//! `$XDG_CONFIG_HOME/roy/cookie` (mode 0600 on Unix) and replayed by
//! `whoami`.
//!
//! `reset` is a local admin escape hatch: it opens the agents DB directly
//! and resets the password of an existing user via `roy_auth::UserStore`. It
//! requires no server and no current session — useful when no one can log
//! in.

use std::path::PathBuf;

/// Resolve a password value with graceful fallbacks. When `prompt_confirm` is
/// true and the password isn't supplied non-interactively, the interactive
/// prompt asks twice and verifies they match. The returned string is always
/// trimmed — callers can use the value directly.
fn resolve_password(
    flag: Option<&str>,
    prompt: &str,
    prompt_confirm: bool,
) -> anyhow::Result<String> {
    use std::io::IsTerminal;
    if let Some(p) = flag {
        return Ok(p.trim().to_string());
    }
    if let Ok(p) = std::env::var("ROY_NEW_PASSWORD") {
        return Ok(p.trim().to_string());
    }
    if !std::io::stdin().is_terminal() {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        return Ok(line.trim().to_string());
    }
    let pw = rpassword::prompt_password(prompt)?;
    if prompt_confirm {
        let confirm = rpassword::prompt_password("confirm password: ")?;
        if pw != confirm {
            anyhow::bail!("passwords don't match");
        }
    }
    Ok(pw.trim().to_string())
}

/// Open the shared agents.db with the same options the daemon uses. Both
/// `create_user` and `reset_password` need this pool — without the helper
/// the boilerplate copies drift.
async fn open_agents_pool() -> anyhow::Result<sqlx::SqlitePool> {
    let db = roy_management::db::default_db_path();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&db)
                .create_if_missing(false)
                .foreign_keys(true),
        )
        .await?;
    Ok(pool)
}

pub fn cookie_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("roy")
        .join("cookie")
}

fn ensure_dir(p: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

pub async fn login(api: &str) -> anyhow::Result<()> {
    use std::io::Write;
    print!("username: ");
    std::io::stdout().flush()?;
    let mut username = String::new();
    std::io::stdin().read_line(&mut username)?;
    let username = username.trim();
    let password = rpassword::prompt_password("password: ")?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{api}/auth/login"))
        .json(&serde_json::json!({"username": username, "password": password}))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("login failed: {}", resp.status());
    }
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .ok_or_else(|| anyhow::anyhow!("no set-cookie"))?
        .to_str()?
        .to_string();
    let path = cookie_path();
    ensure_dir(&path)?;
    std::fs::write(&path, cookie)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(&path)?.permissions();
        p.set_mode(0o600);
        std::fs::set_permissions(&path, p)?;
    }
    eprintln!("Logged in. Cookie saved to {}", path.display());
    Ok(())
}

pub async fn whoami(api: &str) -> anyhow::Result<()> {
    let cookie = std::fs::read_to_string(cookie_path())?;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{api}/auth/me"))
        .header(reqwest::header::COOKIE, cookie)
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("not logged in: {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

/// Local admin escape hatch: provision a fresh user directly against the
/// shared agents.db, bypassing the HTTP layer. Same security model as
/// `reset_password` — anyone who can read/write the DB file can already
/// create users; this is just the ergonomic surface.
///
/// Password resolution order:
///   1. `--password` flag (most explicit, but visible in `ps`)
///   2. `ROY_NEW_PASSWORD` env var (scripted, not in `ps`)
///   3. Non-TTY stdin (one line — useful for `echo pw | roy auth create …`)
///   4. Interactive rpassword prompt (echo-off, requires controlling tty)
pub async fn create_user(
    username: &str,
    display_name: Option<&str>,
    password: Option<&str>,
) -> anyhow::Result<()> {
    let new_pw = resolve_password(password, "new password: ", true)?;
    if new_pw.len() < 8 {
        anyhow::bail!("password too short (min 8)");
    }
    let pool = open_agents_pool().await?;
    let user = roy_auth::UserStore::new(pool)
        .create(roy_auth::NewUser {
            username: username.into(),
            display_name: display_name.unwrap_or(username).into(),
            password: new_pw,
            timezone: None,
        })
        .await?;
    println!("Created user {} (id={})", user.username, user.id);
    Ok(())
}

pub async fn reset_password(username: &str, password: Option<&str>) -> anyhow::Result<()> {
    let new_pw = resolve_password(password, "new password: ", true)?;
    if new_pw.len() < 8 {
        anyhow::bail!("password too short (min 8)");
    }
    let pool = open_agents_pool().await?;
    let user = roy_auth::UserStore::new(pool.clone())
        .get_by_username(username)
        .await?;
    roy_auth::UserStore::new(pool)
        .set_password(&user.id, &new_pw)
        .await?;
    println!("Password updated for {username}");
    Ok(())
}
