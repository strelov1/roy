-- roy-management owns three concerns on top of the shared agents.db pool:
--   * projects: a named workspace directory, owned by a user (optional team).
--   * session_meta: per-session enrichment (project, agent persona, display
--     label, attached connections). Joined with the daemon's sessions.db on
--     session_id at query time.
--   * session_tags: free-form key/value tags, cascaded with session_meta.
--   * connections: user-owned MCP-server bindings, optionally backed by a
--     YAML provider catalog entry.

CREATE TABLE projects (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    path       TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES users(id),
    team_id    TEXT REFERENCES teams(id),
    created_at INTEGER NOT NULL
);

CREATE TABLE session_meta (
    session_id     TEXT PRIMARY KEY,
    project_id     TEXT REFERENCES projects(id) ON DELETE SET NULL,
    agent_id       TEXT,
    agent_name     TEXT,
    display_label  TEXT,
    created_by     TEXT NOT NULL REFERENCES users(id),
    team_id        TEXT REFERENCES teams(id),
    connection_ids TEXT NOT NULL DEFAULT '[]',
    created_at     INTEGER NOT NULL
);
CREATE INDEX session_meta_project ON session_meta(project_id);

CREATE TABLE session_tags (
    session_id TEXT NOT NULL REFERENCES session_meta(session_id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    PRIMARY KEY (session_id, key)
);
CREATE INDEX session_tags_key_value ON session_tags(key, value);

CREATE TABLE connections (
    id           TEXT PRIMARY KEY,
    owner_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    slug         TEXT NOT NULL,
    kind         TEXT NOT NULL,
    config_json  TEXT NOT NULL,
    secrets_json TEXT,
    description  TEXT,
    provider_id  TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    UNIQUE (owner_id, slug)
);
CREATE INDEX connections_owner_idx    ON connections(owner_id);
CREATE INDEX connections_provider_idx ON connections(provider_id);
CREATE UNIQUE INDEX connections_owner_provider_label_unique
  ON connections(owner_id, provider_id, name)
  WHERE provider_id IS NOT NULL;
