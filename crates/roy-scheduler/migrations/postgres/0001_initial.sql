-- spec §4 in Postgres dialect. Mirrored from migrations/sqlite/0001_initial.sql;
-- maintained in lock-step per spec §6.1. Not run in v1.

CREATE TABLE agents (
  id                       TEXT PRIMARY KEY,
  name                     TEXT NOT NULL,
  preset                   TEXT NOT NULL,
  project_id               TEXT,
  task                     TEXT NOT NULL,
  model                    TEXT,
  persistent               BOOLEAN NOT NULL DEFAULT FALSE,
  persistent_session_id    TEXT,
  created_at               TIMESTAMPTZ NOT NULL,
  updated_at               TIMESTAMPTZ NOT NULL
);

CREATE TABLE triggers (
  id              TEXT PRIMARY KEY,
  agent_id        TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  kind            TEXT NOT NULL CHECK(kind IN ('cron','oneshot')),
  cron_expr       TEXT,
  timezone        TEXT NOT NULL DEFAULT 'UTC',
  fire_at         TIMESTAMPTZ,
  next_fire_at    TIMESTAMPTZ NOT NULL,
  last_fire_at    TIMESTAMPTZ,
  paused          BOOLEAN NOT NULL DEFAULT FALSE,
  last_error      TEXT,
  created_at      TIMESTAMPTZ NOT NULL
);
CREATE INDEX triggers_due_idx ON triggers(paused, next_fire_at);

CREATE TABLE fires (
  id                          TEXT PRIMARY KEY,
  agent_id                    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
  trigger_id                  TEXT REFERENCES triggers(id) ON DELETE SET NULL,
  session_id                  TEXT,
  status                      TEXT NOT NULL CHECK(status IN ('running','ok','error','timeout')),
  started_at                  TIMESTAMPTZ NOT NULL,
  finished_at                 TIMESTAMPTZ,
  transcript_seq_range_start  BIGINT,
  transcript_seq_range_end    BIGINT,
  assistant_text              TEXT,
  cost_usd                    DOUBLE PRECISION,
  stop_reason                 TEXT,
  error_message               TEXT
);
CREATE INDEX fires_agent_idx ON fires(agent_id, started_at DESC);

CREATE TABLE fire_subscribers (
  id            TEXT PRIMARY KEY,
  agent_id      TEXT REFERENCES agents(id)   ON DELETE CASCADE,
  trigger_id    TEXT REFERENCES triggers(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK(kind IN ('inject_parent','webhook','notify_native','chain_agent')),
  config        JSONB NOT NULL,
  enabled       BOOLEAN NOT NULL DEFAULT TRUE,
  order_index   INTEGER NOT NULL DEFAULT 0,
  created_at    TIMESTAMPTZ NOT NULL,
  CHECK (agent_id IS NOT NULL OR trigger_id IS NOT NULL)
);
CREATE INDEX fire_subscribers_agent_idx   ON fire_subscribers(agent_id,   enabled);
CREATE INDEX fire_subscribers_trigger_idx ON fire_subscribers(trigger_id, enabled);

CREATE TABLE fire_subscriber_runs (
  id                TEXT PRIMARY KEY,
  fire_id           TEXT NOT NULL REFERENCES fires(id) ON DELETE CASCADE,
  subscriber_id     TEXT NOT NULL REFERENCES fire_subscribers(id) ON DELETE CASCADE,
  status            TEXT NOT NULL CHECK(status IN ('ok','error','skipped')),
  started_at        TIMESTAMPTZ NOT NULL,
  finished_at       TIMESTAMPTZ,
  error_message     TEXT,
  response_snippet  TEXT
);
CREATE INDEX fire_subscriber_runs_fire_idx ON fire_subscriber_runs(fire_id);
