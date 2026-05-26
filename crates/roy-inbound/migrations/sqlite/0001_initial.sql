CREATE TABLE bindings (
    id              TEXT PRIMARY KEY,
    source_id       TEXT NOT NULL,
    sender_id       TEXT NOT NULL,
    session_id      TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    strategy        TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    last_active_at  TEXT NOT NULL,
    UNIQUE(source_id, sender_id)
);

CREATE INDEX bindings_by_last_active ON bindings(last_active_at);
