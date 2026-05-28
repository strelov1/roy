-- Rename `agents.preset` to `agents.harness` to align with the project-wide
-- terminology refactor (preset → harness). The column held one of
-- claude/gemini/opencode/codex/pi — that is a harness identifier, not a
-- preset.
ALTER TABLE agents RENAME COLUMN preset TO harness;
