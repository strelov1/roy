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
