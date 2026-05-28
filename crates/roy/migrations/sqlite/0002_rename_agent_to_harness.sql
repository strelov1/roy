-- Rename the boot-kit column `sessions.agent` to `sessions.harness` to align
-- with the project-wide terminology refactor (preset/agent-as-binary → harness).
-- The persona-style "agent" concept moved to file-based discovery; this column
-- always held the harness identifier (claude, gemini, opencode, codex, pi).
ALTER TABLE sessions RENAME COLUMN agent TO harness;
