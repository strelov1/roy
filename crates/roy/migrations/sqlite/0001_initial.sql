CREATE TABLE sessions (
  session_id     TEXT PRIMARY KEY,
  harness        TEXT NOT NULL,
  cwd            TEXT NOT NULL,
  model          TEXT,
  permission     TEXT,
  resume_cursor  TEXT,
  system_prompt  TEXT,
  created_at     INTEGER NOT NULL,
  closed_at      INTEGER
);

CREATE INDEX sessions_live ON sessions(closed_at) WHERE closed_at IS NULL;
