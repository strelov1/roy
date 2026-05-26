-- 0005_owners.sql
--
-- Add owner columns (`created_by`, `team_id`) to `projects` and
-- `session_meta`. SQLite can't add NOT NULL FK columns to existing tables
-- without a default, so we drop and recreate. Any pre-existing rows in
-- `projects`, `session_meta`, and `session_tags` are wiped — the user
-- model is new and no production deployment carries data that needs to
-- survive this migration.
--
-- `projects.name` keeps its UNIQUE constraint (matches 0004).
-- `session_tags` is recreated with the explicit `REFERENCES session_meta`
-- + `ON DELETE CASCADE` it always wanted but couldn't express until the
-- parent table was rewritten.

DELETE FROM session_tags;
DELETE FROM session_meta;
DELETE FROM projects;

DROP TABLE session_tags;
DROP TABLE session_meta;
DROP TABLE projects;

CREATE TABLE projects (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    path       TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES users(id),
    team_id    TEXT REFERENCES teams(id),
    created_at INTEGER NOT NULL
);

CREATE TABLE session_meta (
    session_id    TEXT PRIMARY KEY,
    project_id    TEXT REFERENCES projects(id) ON DELETE SET NULL,
    agent_id      TEXT,
    agent_name    TEXT,
    display_label TEXT,
    created_by    TEXT NOT NULL REFERENCES users(id),
    team_id       TEXT REFERENCES teams(id),
    created_at    INTEGER NOT NULL
);
CREATE INDEX session_meta_project ON session_meta(project_id);

CREATE TABLE session_tags (
    session_id TEXT NOT NULL REFERENCES session_meta(session_id) ON DELETE CASCADE,
    key        TEXT NOT NULL,
    value      TEXT NOT NULL,
    PRIMARY KEY (session_id, key)
);
CREATE INDEX session_tags_key_value ON session_tags(key, value);
