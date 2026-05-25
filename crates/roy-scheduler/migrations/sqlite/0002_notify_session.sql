-- Optional roy session to notify. When set, the scheduler appends a
-- `roy inject <notify_session> ...` instruction to the agent's fired prompt.
ALTER TABLE agents ADD COLUMN notify_session TEXT;
