-- roy-auth owns users, teams, team memberships, and team invites. All of these
-- live in the same agents.db SQLite file as roy-management's metadata tables;
-- the sqlx Migrator is run with `set_ignore_missing(true)` so sibling crates
-- coexist in the shared `_sqlx_migrations` table.

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    timezone      TEXT,
    created_at    INTEGER NOT NULL
);

CREATE TABLE teams (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT,
    created_by  TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE team_members (
    user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    team_id   TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    role      TEXT NOT NULL DEFAULT 'member',
    joined_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, team_id)
);
CREATE INDEX team_members_by_team ON team_members(team_id);

CREATE TABLE team_invites (
    token        TEXT PRIMARY KEY,
    team_id      TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    created_by   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,
    accepted_by  TEXT REFERENCES users(id) ON DELETE SET NULL,
    accepted_at  INTEGER
);
