// crates/roy-management/src/db.rs
//
// Shared SQLite path for roy-cli + roy-management. Was previously in the
// roy-agents crate; that crate is being deleted now that agents live in
// `~/.roy/agents/*.md` files.

use std::path::PathBuf;

/// `$ROY_AGENTS_DB`, else `~/.local/state/roy/agents.db`.
pub fn default_db_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ROY_AGENTS_DB") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy/agents.db")
}
