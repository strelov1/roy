CREATE TABLE team_invites (
    token        TEXT PRIMARY KEY,
    team_id      TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    created_by   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,
    accepted_by  TEXT REFERENCES users(id) ON DELETE SET NULL,
    accepted_at  INTEGER
);
