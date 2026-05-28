//! User-owned provider catalog. Reads `~/.roy/connections.yaml` once at
//! startup. The HTTP `/providers` endpoint serves the same in-memory copy
//! to every caller (no per-request file I/O).
//!
//! Boot policy:
//! * Missing file → empty catalog. Users who don't use MCP connections
//!   never need to think about the file.
//! * Broken file (exists but malformed) → load returns `Err(CatalogError)`;
//!   `lib.rs::run` propagates this as a fatal startup error.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One provider definition from the YAML catalog. Mirrors the spec's schema
/// directly — fields are renamed to match the on-disk format via `serde`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<SecretSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SecretSchema {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("validation error in {path}: {reason}")]
    Schema { path: PathBuf, reason: String },
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    providers: Vec<Provider>,
}

impl Catalog {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Constructor for tests that want a pre-loaded catalog without writing
    /// a yaml file. Production code goes through `load_from`.
    #[doc(hidden)]
    pub fn from_providers(providers: Vec<Provider>) -> Self {
        Self { providers }
    }

    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }

    pub fn get(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Load from `path`. Missing file → empty catalog. Broken file → Err.
    pub fn load_from(path: &Path) -> Result<Self, CatalogError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty());
            }
            Err(e) => {
                return Err(CatalogError::Io {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        let providers: Vec<Provider> =
            serde_yaml::from_str(&text).map_err(|e| CatalogError::Parse {
                path: path.to_path_buf(),
                source: e,
            })?;
        for (i, p) in providers.iter().enumerate() {
            if p.id.is_empty() {
                return Err(CatalogError::Schema {
                    path: path.to_path_buf(),
                    reason: format!("entry #{i}: `id` is empty"),
                });
            }
            if p.command.is_empty() {
                return Err(CatalogError::Schema {
                    path: path.to_path_buf(),
                    reason: format!("entry `{}`: `command` is empty", p.id),
                });
            }
        }
        // Reject duplicate ids — silent overwrite is worse than a startup error.
        let mut seen = std::collections::HashSet::new();
        for p in &providers {
            if !seen.insert(p.id.clone()) {
                return Err(CatalogError::Schema {
                    path: path.to_path_buf(),
                    reason: format!("duplicate provider id `{}`", p.id),
                });
            }
        }
        Ok(Self { providers })
    }
}

/// Default path: `~/.roy/connections.yaml`. Mirrors how the rest of the
/// codebase resolves `~/.roy/*` (via `dirs::home_dir`).
pub fn default_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".roy/connections.yaml")
}

/// The default catalog shipped in the repo (`resources/connections.default.yaml`),
/// available at compile time. Used by tests and as a reference path for the
/// boot-error message.
pub const DEFAULT_CATALOG_YAML: &str = include_str!("../resources/connections.default.yaml");
