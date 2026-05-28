-- 0007_connections_provider_id.sql
--
-- Wires the YAML provider catalog into the `connections` table.
-- `provider_id` is a string FK by name into `~/.roy/connections.yaml` (the
-- catalog is read-only, lives outside the DB — no real FK constraint to
-- enforce, just a soft reference).
--
-- The partial UNIQUE index enforces "one (provider, label) per owner" for
-- catalog-backed rows. Legacy free-form rows (provider_id IS NULL) are
-- excluded so existing connections aren't constrained.

ALTER TABLE connections ADD COLUMN provider_id TEXT;
CREATE INDEX connections_provider_idx ON connections(provider_id);
CREATE UNIQUE INDEX connections_owner_provider_label_unique
  ON connections(owner_id, provider_id, name)
  WHERE provider_id IS NOT NULL;
