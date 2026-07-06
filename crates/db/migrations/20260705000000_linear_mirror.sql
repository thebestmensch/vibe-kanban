-- JM-718: outbound Linear status mirror.
--
-- Links a board card (`issues` row) to a Linear issue and marks it for an
-- outbound status push. Linear ids/identifiers/urls/state-ids are opaque
-- external strings (not our BLOB UUIDs), so they are stored as TEXT.
--
-- `linear_sync_pending` is the outbound drain flag (mirrors `pull_requests`'
-- pending-sync pattern): set to 1 ONLY by the user-driven mutation path when a
-- linked card's `status_id` actually changes; cleared by the sync loop after a
-- successful push. The sync loop's own writes never set it (echo-loop invariant,
-- ADR 0002).

-- Which Linear account this project's cards sync to (config key; NULL = unbound).
ALTER TABLE projects ADD COLUMN linear_account_key TEXT;

-- Card -> Linear issue link + outbound drain flag.
ALTER TABLE issues ADD COLUMN linear_issue_id         TEXT;
ALTER TABLE issues ADD COLUMN linear_issue_identifier TEXT;
ALTER TABLE issues ADD COLUMN linear_url              TEXT;
ALTER TABLE issues ADD COLUMN linear_state_id         TEXT;
ALTER TABLE issues ADD COLUMN linear_sync_pending     INTEGER NOT NULL DEFAULT 0;

-- Dedup backstop: a Linear issue links to at most one card. Partial so the many
-- unlinked cards (NULL) are exempt. Also lets inbound import (JM-734) use
-- INSERT ... ON CONFLICT DO NOTHING instead of check-then-insert.
CREATE UNIQUE INDEX idx_issues_linear_issue_id
    ON issues(linear_issue_id) WHERE linear_issue_id IS NOT NULL;

-- Fast outbound drain: only the handful of pending rows are indexed.
CREATE INDEX idx_issues_linear_sync_pending
    ON issues(linear_sync_pending) WHERE linear_sync_pending = 1;
