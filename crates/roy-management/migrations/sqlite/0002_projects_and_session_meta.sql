CREATE TABLE projects (
  id         TEXT PRIMARY KEY,
  name       TEXT UNIQUE NOT NULL,
  path       TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE session_meta (
  session_id    TEXT PRIMARY KEY,
  project_id    TEXT REFERENCES projects(id) ON DELETE SET NULL,
  agent_id      TEXT REFERENCES agents(id) ON DELETE SET NULL,
  agent_name    TEXT,
  display_label TEXT,
  created_at    INTEGER NOT NULL
);
CREATE INDEX session_meta_project ON session_meta(project_id);

CREATE TABLE session_tags (
  session_id TEXT NOT NULL,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL,
  PRIMARY KEY (session_id, key)
);
CREATE INDEX session_tags_key_value ON session_tags(key, value);
