//! `POST /uploads` — single-file upload from the web UI.
//!
//! Files land in `~/.roy/uploads/<user_id>/<uuid>-<safe-name>` and the
//! handler returns the absolute path so the client can splice it into the
//! prompt as `@/abs/path`. The daemon's agents run on the same host under
//! the same OS user, so they can read this path directly.
//!
//! Body size is capped externally via `DefaultBodyLimit` on the route — the
//! `Multipart` extractor itself doesn't enforce a ceiling.

use axum::{
    extract::Multipart,
    http::StatusCode,
    Extension, Json,
};
use serde::Serialize;
use tokio::io::AsyncWriteExt;

use crate::auth::AuthUser;
use crate::http::ApiError;

/// Max length of the sanitised display fragment in the on-disk name. The
/// uuid prefix already guarantees uniqueness; this cap is purely to keep
/// paths readable in logs.
const NAME_LEN_CAP: usize = 80;

#[derive(Serialize)]
pub struct UploadResp {
    /// Absolute path on the daemon host — what the client splices as `@/path`.
    pub path: String,
    /// Original filename as reported by the browser (for UI display).
    pub name: String,
    /// Size in bytes of the stored file.
    pub size: u64,
}

pub async fn upload(
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    mut mp: Multipart,
) -> Result<Json<UploadResp>, ApiError> {
    let field = mp
        .next_field()
        .await
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("multipart: {e}")))?
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "no file field".into()))?;

    let original = field
        .file_name()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "file".into());
    let safe = sanitise_name(&original);

    let bytes = field
        .bytes()
        .await
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("read body: {e}")))?;

    let home = dirs::home_dir()
        .ok_or_else(|| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "no home dir".into()))?;
    let dir = home.join(".roy").join("uploads").join(&user_id);
    tokio::fs::create_dir_all(&dir).await.map_err(|e| {
        tracing::error!(error = %e, path = %dir.display(), "create upload dir");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    })?;

    let id = uuid::Uuid::new_v4();
    let file_name = format!("{id}-{safe}");
    let path = dir.join(&file_name);

    let mut f = tokio::fs::File::create(&path).await.map_err(|e| {
        tracing::error!(error = %e, path = %path.display(), "create upload file");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    })?;
    f.write_all(&bytes).await.map_err(|e| {
        tracing::error!(error = %e, path = %path.display(), "write upload");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    })?;
    // Without a clean flush the kernel may still hold buffered bytes —
    // returning success here would advertise a truncated file to the agent.
    f.flush().await.map_err(|e| {
        tracing::error!(error = %e, path = %path.display(), "flush upload");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
    })?;

    Ok(Json(UploadResp {
        path: path.to_string_lossy().into_owned(),
        name: original,
        size: bytes.len() as u64,
    }))
}

/// Replace anything outside `[A-Za-z0-9._-]` with `_`, strip leading dots
/// (no hidden files), and cap length. The result is guaranteed safe to use
/// as a single path segment.
fn sanitise_name(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(NAME_LEN_CAP));
    for ch in input.chars() {
        let safe = matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-');
        out.push(if safe { ch } else { '_' });
        if out.len() >= NAME_LEN_CAP {
            break;
        }
    }
    let trimmed = out.trim_start_matches('.');
    if trimmed.is_empty() {
        "file".into()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::sanitise_name;

    #[test]
    fn strips_path_traversal() {
        // Leading dots get trimmed; slashes become underscores. Net result
        // is a single path segment that cannot escape the upload dir.
        assert_eq!(sanitise_name("../etc/passwd"), "_etc_passwd");
        assert_eq!(sanitise_name("/abs/path.txt"), "_abs_path.txt");
    }

    #[test]
    fn preserves_safe_chars() {
        assert_eq!(sanitise_name("README.md"), "README.md");
        assert_eq!(sanitise_name("My-File_v2.txt"), "My-File_v2.txt");
    }

    #[test]
    fn strips_leading_dots() {
        assert_eq!(sanitise_name(".env"), "env");
        assert_eq!(sanitise_name("..hidden"), "hidden");
    }

    #[test]
    fn empty_becomes_file() {
        assert_eq!(sanitise_name(""), "file");
        assert_eq!(sanitise_name("....."), "file");
    }

    #[test]
    fn caps_length() {
        let long: String = "a".repeat(200);
        assert_eq!(sanitise_name(&long).len(), 80);
    }
}
