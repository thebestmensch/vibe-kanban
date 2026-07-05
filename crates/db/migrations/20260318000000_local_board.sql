-- JM-714 v1: local-SQLite kanban board.
-- Ports the cloud board schema (crates/remote 20260112000000_remote-projects.sql)
-- to SQLite so the board runs on local data. Keeps entity shapes verbatim for
-- ts-rs parity with api_types (Issue / ProjectStatus / Project). Divergences are
-- storage-only: BLOB UUIDs, TEXT timestamps/JSON, TEXT+CHECK in place of PG ENUMs,
-- REAL sort_order (fractional indexing). simple_id is assigned in app code inside a
-- transaction (see crates/db/src/models/issue.rs), not a trigger.

-- 1. ORGANIZATIONS -----------------------------------------------------------
-- Single-user fork: exactly one org row, so the issue_prefix lookup and the
-- projects.organization_id FK keep meaning. Fixed UUID 0…01 (see LOCAL_ORG_ID).
CREATE TABLE organizations (
    id           BLOB PRIMARY KEY,
    name         TEXT NOT NULL,
    issue_prefix TEXT NOT NULL DEFAULT 'VK',
    created_at   TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now', 'subsec'))
);

INSERT INTO organizations (id, name, issue_prefix)
VALUES (X'00000000000000000000000000000001', 'Local', 'VK');

-- 2. USERS -------------------------------------------------------------------
-- Single seeded user; the shell (OrgProvider/UserProvider replacement) renders a
-- current-user identity and issues carry a nullable creator_user_id. Fixed UUID 0…02.
CREATE TABLE users (
    id           BLOB PRIMARY KEY,
    name         TEXT NOT NULL,
    email        TEXT,
    avatar_url   TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now', 'subsec'))
);

INSERT INTO users (id, name, email)
VALUES (X'00000000000000000000000000000002', 'Local User', NULL);

-- 3. EXTEND PROJECTS ---------------------------------------------------------
-- Add the board-domain columns the api_types::Project shape expects. SQLite can't
-- ADD COLUMN with a subquery default, so add nullable then backfill to the org.
ALTER TABLE projects ADD COLUMN organization_id BLOB
    REFERENCES organizations(id) ON DELETE CASCADE;
ALTER TABLE projects ADD COLUMN color         TEXT NOT NULL DEFAULT '0 0% 0%';
ALTER TABLE projects ADD COLUMN sort_order    INTEGER NOT NULL DEFAULT 0;
ALTER TABLE projects ADD COLUMN issue_counter INTEGER NOT NULL DEFAULT 0;

UPDATE projects
SET organization_id = X'00000000000000000000000000000001'
WHERE organization_id IS NULL;

-- 4. PROJECT STATUSES --------------------------------------------------------
CREATE TABLE project_statuses (
    id         BLOB PRIMARY KEY,
    project_id BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    color      TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    hidden     INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now', 'subsec'))
);

CREATE INDEX idx_project_statuses_project_id ON project_statuses(project_id);

-- Seed the default column set for every existing project so the board renders.
-- Mirrors the local TaskStatus set (todo / inprogress / inreview / done / cancelled).
-- New-project seeding is wired in the project-create path (crates/db seed_defaults).
INSERT INTO project_statuses (id, project_id, name, color, sort_order, hidden)
SELECT randomblob(16), p.id, v.name, v.color, v.sort_order, 0
FROM projects p
CROSS JOIN (
    SELECT 'Todo'        AS name, '220 9% 46%'  AS color, 0 AS sort_order
    UNION ALL SELECT 'In Progress', '38 92% 50%',  1
    UNION ALL SELECT 'In Review',   '221 83% 53%', 2
    UNION ALL SELECT 'Done',        '142 71% 45%', 3
    UNION ALL SELECT 'Cancelled',   '0 84% 60%',   4
) v;

-- 5. ISSUES ------------------------------------------------------------------
CREATE TABLE issues (
    id                      BLOB PRIMARY KEY,
    project_id              BLOB NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    issue_number            INTEGER NOT NULL,
    simple_id               TEXT NOT NULL,
    status_id               BLOB NOT NULL REFERENCES project_statuses(id),
    title                   TEXT NOT NULL,
    description             TEXT,
    priority                TEXT CHECK (priority IN ('urgent', 'high', 'medium', 'low')),
    start_date              TEXT,
    target_date             TEXT,
    completed_at            TEXT,
    sort_order              REAL NOT NULL DEFAULT 0,
    parent_issue_id         BLOB REFERENCES issues(id) ON DELETE SET NULL,
    parent_issue_sort_order REAL,
    extension_metadata      TEXT NOT NULL DEFAULT '{}',
    creator_user_id         BLOB REFERENCES users(id) ON DELETE SET NULL,
    created_at              TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at              TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    -- Backstop for the app-side simple_id counter (multi-tab / retry-after-timeout).
    CONSTRAINT issues_project_issue_number_uniq UNIQUE (project_id, issue_number)
);

CREATE INDEX idx_issues_project_id       ON issues(project_id);
CREATE INDEX idx_issues_status_id        ON issues(status_id);
CREATE INDEX idx_issues_parent_issue_id  ON issues(parent_issue_id);
CREATE INDEX idx_issues_simple_id        ON issues(simple_id);
