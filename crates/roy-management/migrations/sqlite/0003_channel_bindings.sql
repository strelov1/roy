CREATE TABLE channel_bindings (
    id                TEXT PRIMARY KEY,
    owner_id          TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_kind      TEXT NOT NULL,
    connection_id     TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    agent_slug        TEXT NOT NULL,
    agent_scope       TEXT NOT NULL,
    session_strategy  TEXT NOT NULL,
    idle_timeout_secs INTEGER,
    allowed_user_ids  TEXT,
    enabled           INTEGER NOT NULL DEFAULT 1,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    UNIQUE (connection_id)
);
CREATE INDEX channel_bindings_owner_idx ON channel_bindings(owner_id);
