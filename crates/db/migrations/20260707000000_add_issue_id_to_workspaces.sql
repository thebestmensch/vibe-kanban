-- JM-749: link a locally-spawned workspace back to the board issue it was
-- created from, so the local board card can join issue → workspace → pull
-- request and render a CI-check badge (the remote ElectricSQL layer carries
-- workspace.issue_id; locally it did not exist). NULL for ad-hoc workspaces and
-- every pre-existing row (no backfill).
--
-- BLOB (not TEXT): every UUID column in this schema is stored as a BLOB
-- (workspaces.id/task_id, issues.id, pull_requests.workspace_id). sqlx binds
-- `Uuid` as a BLOB, so the join `workspaces.issue_id = issues.id` only matches
-- reliably when both sides share BLOB storage — a TEXT column would risk
-- silently missing rows.
ALTER TABLE workspaces ADD COLUMN issue_id BLOB;

-- The board fallback query filters workspaces by their linked issue's project,
-- joining workspaces → issues on this column, then pull_requests → workspaces.
CREATE INDEX idx_workspaces_issue_id ON workspaces(issue_id);
