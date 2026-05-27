-- 0006_connections.sql
--
-- User-owned MCP-server connections. One row = one upstream MCP the user has
-- registered. Inline credentials live in `secrets_json` (plain JSON, file
-- mode 0600 already enforced by roy-agents::open). A follow-up plan will add
-- column-level encryption.
--
-- `kind` is reserved for future transports (mcp_http, mcp_sse, ...). MVP
-- accepts only 'mcp_stdio'; other values are rejected by the store layer.

CREATE TABLE connections (
    id           TEXT PRIMARY KEY,
    owner_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    slug         TEXT NOT NULL,
    kind         TEXT NOT NULL,
    config_json  TEXT NOT NULL,
    secrets_json TEXT,
    description  TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    UNIQUE (owner_id, slug)
);
CREATE INDEX connections_owner_idx ON connections(owner_id);

-- session_meta needs a column to recall which connections a session was
-- spawned with — required for /sessions GET (UI display) and for future
-- resume support. JSON array of connection ids; empty array = no connections.
ALTER TABLE session_meta ADD COLUMN connection_ids TEXT NOT NULL DEFAULT '[]';
