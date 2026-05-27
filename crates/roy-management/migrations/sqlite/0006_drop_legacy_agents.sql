-- The roy-agents crate has been removed; agents now live as files in
-- ~/.roy/agents/<name>.md. Drop the legacy table on existing deployments
-- so the DB doesn't carry around orphaned rows. Fresh deployments never
-- had the table (those migrations went away with the crate).
DROP TABLE IF EXISTS agents;
