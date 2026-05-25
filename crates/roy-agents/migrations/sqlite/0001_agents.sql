-- The canonical agent store, shared by roy-management (interactive, uses
-- `prompt`) and, later, roy-scheduler (scheduled, uses `task`). Superset schema
-- so the scheduler migration needs no schema change. Created mode 0600 in db.rs.
CREATE TABLE agents (
  id          TEXT PRIMARY KEY,
  name        TEXT NOT NULL,
  slug        TEXT NOT NULL UNIQUE,
  description TEXT,
  preset      TEXT NOT NULL,
  model       TEXT,
  prompt      TEXT NOT NULL DEFAULT '',
  task        TEXT,
  persistent  INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX agents_created_idx ON agents(created_at DESC);
