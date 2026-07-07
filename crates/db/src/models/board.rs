//! Local-SQLite persistence for the kanban board (JM-714 v1).
//!
//! Re-backs the board entities that used to live in the cloud (`crates/remote`)
//! against the local SQLite pool, producing the exact `api_types` DTOs so the
//! `/api/remote/*` routes keep their contract without an Electric/Postgres round
//! trip. Storage diverges from the DTOs only where SQLite forces it (JSON stored
//! as TEXT, timestamps as TEXT, UUIDs as BLOB); `IssueRow::into_api` bridges the
//! one lossy field (`extension_metadata`).

use api_types::{
    CreateIssueRequest, CreateProjectRequest, CreateProjectStatusRequest, Issue, IssuePriority,
    ListIssuesResponse, Project, ProjectStatus, SearchIssuesRequest, UpdateIssueRequest,
    UpdateProjectRequest, UpdateProjectStatusRequest,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::SqlitePool;
use uuid::Uuid;

use super::merge::{CheckStatus, MergeStatus};

/// Default column set seeded for every project so the board renders. Mirrors the
/// local `TaskStatus` set (todo / inprogress / inreview / done / cancelled).
const DEFAULT_STATUSES: &[(&str, &str)] = &[
    ("Todo", "220 9% 46%"),
    ("In Progress", "38 92% 50%"),
    ("In Review", "221 83% 53%"),
    ("Done", "142 71% 45%"),
    ("Cancelled", "0 84% 60%"),
];

// ---------------------------------------------------------------------------
// Projects (board shape)
// ---------------------------------------------------------------------------

/// Namespace for reading local projects in the cloud `api_types::Project` shape.
pub struct BoardProjects;

impl BoardProjects {
    pub async fn list_by_org(
        pool: &SqlitePool,
        organization_id: Uuid,
    ) -> Result<Vec<Project>, sqlx::Error> {
        sqlx::query_as!(
            Project,
            r#"SELECT id                as "id!: Uuid",
                      organization_id   as "organization_id!: Uuid",
                      name,
                      color             as "color!",
                      sort_order        as "sort_order!: i32",
                      created_at        as "created_at!: DateTime<Utc>",
                      updated_at        as "updated_at!: DateTime<Utc>"
               FROM projects
               WHERE organization_id = $1
               ORDER BY sort_order ASC, created_at ASC"#,
            organization_id
        )
        .fetch_all(pool)
        .await
    }

    pub async fn get(pool: &SqlitePool, id: Uuid) -> Result<Option<Project>, sqlx::Error> {
        sqlx::query_as!(
            Project,
            r#"SELECT id                as "id!: Uuid",
                      organization_id   as "organization_id!: Uuid",
                      name,
                      color             as "color!",
                      sort_order        as "sort_order!: i32",
                      created_at        as "created_at!: DateTime<Utc>",
                      updated_at        as "updated_at!: DateTime<Utc>"
               FROM projects
               WHERE id = $1"#,
            id
        )
        .fetch_optional(pool)
        .await
    }

    /// Create a local board (project). Forces `organization_id = LOCAL_ORG_ID`
    /// (single-org invariant): the board's project list filters by org, so a new
    /// board must carry the local org to appear — we ignore any client-sent org
    /// rather than trust the optimistic row. Seeds the default column set in the
    /// same transaction so a fresh board renders with columns, and appends to the
    /// end of the org's ordering. `projects` is the original workspace/task table
    /// (the v1 migration added the board columns), but `git_repo_path` was dropped
    /// in the repos-registry migration, so a repo-less board row is valid.
    pub async fn create(
        pool: &SqlitePool,
        req: &CreateProjectRequest,
    ) -> Result<Project, sqlx::Error> {
        let id = req.id.unwrap_or_else(Uuid::new_v4);
        let org_id = crate::LOCAL_ORG_ID;

        let mut tx = pool.begin().await?;

        let sort_order = sqlx::query_scalar!(
            r#"SELECT COALESCE(MAX(sort_order), -1) + 1 as "next!: i32"
               FROM projects WHERE organization_id = $1"#,
            org_id
        )
        .fetch_one(&mut *tx)
        .await?;

        let project = sqlx::query_as!(
            Project,
            r#"INSERT INTO projects (id, name, organization_id, color, sort_order)
               VALUES ($1, $2, $3, $4, $5)
               RETURNING id              as "id!: Uuid",
                         organization_id as "organization_id!: Uuid",
                         name,
                         color           as "color!",
                         sort_order      as "sort_order!: i32",
                         created_at      as "created_at!: DateTime<Utc>",
                         updated_at      as "updated_at!: DateTime<Utc>""#,
            id,
            req.name,
            org_id,
            req.color,
            sort_order
        )
        .fetch_one(&mut *tx)
        .await?;

        for (idx, (name, color)) in DEFAULT_STATUSES.iter().enumerate() {
            let sid = Uuid::new_v4();
            let so = idx as i32;
            sqlx::query!(
                r#"INSERT INTO project_statuses (id, project_id, name, color, sort_order, hidden)
                   VALUES ($1, $2, $3, $4, $5, 0)"#,
                sid,
                id,
                name,
                color,
                so
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(project)
    }

    /// Partial update on a caller-supplied connection so `bulk_update` can batch a
    /// whole reorder into one transaction (all-or-nothing). Mirrors the fetch-then
    /// -overwrite idiom used for issues/statuses.
    async fn update_on(
        conn: &mut sqlx::SqliteConnection,
        id: Uuid,
        req: &UpdateProjectRequest,
    ) -> Result<Project, sqlx::Error> {
        let existing = sqlx::query_as!(
            Project,
            r#"SELECT id                as "id!: Uuid",
                      organization_id   as "organization_id!: Uuid",
                      name,
                      color             as "color!",
                      sort_order        as "sort_order!: i32",
                      created_at        as "created_at!: DateTime<Utc>",
                      updated_at        as "updated_at!: DateTime<Utc>"
               FROM projects WHERE id = $1"#,
            id
        )
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;

        let name = req.name.clone().unwrap_or(existing.name);
        let color = req.color.clone().unwrap_or(existing.color);
        let sort_order = req.sort_order.unwrap_or(existing.sort_order);

        sqlx::query_as!(
            Project,
            r#"UPDATE projects
               SET name = $2, color = $3, sort_order = $4,
                   updated_at = datetime('now', 'subsec')
               WHERE id = $1
               RETURNING id              as "id!: Uuid",
                         organization_id as "organization_id!: Uuid",
                         name,
                         color           as "color!",
                         sort_order      as "sort_order!: i32",
                         created_at      as "created_at!: DateTime<Utc>",
                         updated_at      as "updated_at!: DateTime<Utc>""#,
            id,
            name,
            color,
            sort_order
        )
        .fetch_one(&mut *conn)
        .await
    }

    /// Partial update of a single board (rename / recolor).
    pub async fn update(
        pool: &SqlitePool,
        id: Uuid,
        req: &UpdateProjectRequest,
    ) -> Result<Project, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        Self::update_on(&mut conn, id, req).await
    }

    /// Apply several board updates atomically (drag-reorder sends the whole batch
    /// as one optimistic operation; a mid-batch failure must roll back).
    pub async fn bulk_update(
        pool: &SqlitePool,
        updates: &[(Uuid, UpdateProjectRequest)],
    ) -> Result<(), sqlx::Error> {
        let mut tx = pool.begin().await?;
        for (id, req) in updates {
            Self::update_on(&mut tx, *id, req).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Read a project's bound Linear account key (JM-718). `None` = unbound (or
    /// the project does not exist — callers that need that distinction check the
    /// issue/project first).
    pub async fn linear_account_key(
        pool: &SqlitePool,
        id: Uuid,
    ) -> Result<Option<String>, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT linear_account_key FROM projects WHERE id = $1"#,
            id
        )
        .fetch_optional(pool)
        .await?;
        Ok(row.and_then(|r| r.linear_account_key))
    }

    /// Bind (or unbind, with `None`) a project to a Linear account key (JM-718).
    pub async fn set_linear_account_key(
        pool: &SqlitePool,
        id: Uuid,
        key: Option<String>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"UPDATE projects SET linear_account_key = $2 WHERE id = $1"#,
            id,
            key
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Read a project's default Claude executor variant (JM-735). `None` = no
    /// per-project default (fresh spawns fall through to the global default).
    /// Kept off the `api_types::Project` DTO (like `linear_account_key`) so the
    /// remote/Electric project shape is untouched — it is a local-only binding.
    pub async fn claude_account_variant(
        pool: &SqlitePool,
        id: Uuid,
    ) -> Result<Option<String>, sqlx::Error> {
        let row = sqlx::query!(
            r#"SELECT claude_account_variant FROM projects WHERE id = $1"#,
            id
        )
        .fetch_optional(pool)
        .await?;
        Ok(row.and_then(|r| r.claude_account_variant))
    }

    /// Set (or clear, with `None`) a project's default Claude executor variant.
    pub async fn set_claude_account_variant(
        pool: &SqlitePool,
        id: Uuid,
        variant: Option<String>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"UPDATE projects SET claude_account_variant = $2 WHERE id = $1"#,
            id,
            variant
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    // NOTE: no board delete. `projects` is the shared workspace/task table — its
    // `id` is referenced with ON DELETE CASCADE by legacy local subsystems
    // (`tasks`, `task_attempts`, `project_repos`, …), and the v1 migration
    // backfilled `organization_id` onto every existing project, so a workspace
    // project also appears on the board. A plain `DELETE FROM projects` would
    // erase far more than the board's cards/columns, beyond what a "delete board"
    // action implies. Safe board removal needs a board/workspace split or a
    // soft-delete/archive — deferred to a follow-up (see JM-732 review notes).
}

// ---------------------------------------------------------------------------
// Project statuses
// ---------------------------------------------------------------------------

/// Namespace for `project_statuses` CRUD in the `api_types::ProjectStatus` shape.
pub struct ProjectStatuses;

impl ProjectStatuses {
    pub async fn list_by_project(
        pool: &SqlitePool,
        project_id: Uuid,
    ) -> Result<Vec<ProjectStatus>, sqlx::Error> {
        sqlx::query_as!(
            ProjectStatus,
            r#"SELECT id         as "id!: Uuid",
                      project_id as "project_id!: Uuid",
                      name,
                      color      as "color!",
                      sort_order as "sort_order!: i32",
                      hidden     as "hidden!: bool",
                      created_at as "created_at!: DateTime<Utc>"
               FROM project_statuses
               WHERE project_id = $1
               ORDER BY sort_order ASC"#,
            project_id
        )
        .fetch_all(pool)
        .await
    }

    /// Seed the default column set for a project that has none yet. Idempotent:
    /// no-op when the project already has statuses (new-project create path).
    pub async fn seed_defaults(pool: &SqlitePool, project_id: Uuid) -> Result<(), sqlx::Error> {
        let existing = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!: i64" FROM project_statuses WHERE project_id = $1"#,
            project_id
        )
        .fetch_one(pool)
        .await?;
        if existing > 0 {
            return Ok(());
        }

        for (idx, (name, color)) in DEFAULT_STATUSES.iter().enumerate() {
            let id = Uuid::new_v4();
            let sort_order = idx as i32;
            sqlx::query!(
                r#"INSERT INTO project_statuses (id, project_id, name, color, sort_order, hidden)
                   VALUES ($1, $2, $3, $4, $5, 0)"#,
                id,
                project_id,
                name,
                color,
                sort_order
            )
            .execute(pool)
            .await?;
        }
        Ok(())
    }

    pub async fn create(
        pool: &SqlitePool,
        req: &CreateProjectStatusRequest,
    ) -> Result<ProjectStatus, sqlx::Error> {
        let id = req.id.unwrap_or_else(Uuid::new_v4);
        sqlx::query_as!(
            ProjectStatus,
            r#"INSERT INTO project_statuses (id, project_id, name, color, sort_order, hidden)
               VALUES ($1, $2, $3, $4, $5, $6)
               RETURNING id         as "id!: Uuid",
                         project_id as "project_id!: Uuid",
                         name,
                         color      as "color!",
                         sort_order as "sort_order!: i32",
                         hidden     as "hidden!: bool",
                         created_at as "created_at!: DateTime<Utc>""#,
            id,
            req.project_id,
            req.name,
            req.color,
            req.sort_order,
            req.hidden
        )
        .fetch_one(pool)
        .await
    }

    /// Partial update on a caller-supplied connection so `bulk_update` can batch
    /// column reorders into one transaction (all-or-nothing).
    async fn update_on(
        conn: &mut sqlx::SqliteConnection,
        id: Uuid,
        req: &UpdateProjectStatusRequest,
    ) -> Result<ProjectStatus, sqlx::Error> {
        let existing = sqlx::query_as!(
            ProjectStatus,
            r#"SELECT id         as "id!: Uuid",
                      project_id as "project_id!: Uuid",
                      name,
                      color      as "color!",
                      sort_order as "sort_order!: i32",
                      hidden     as "hidden!: bool",
                      created_at as "created_at!: DateTime<Utc>"
               FROM project_statuses WHERE id = $1"#,
            id
        )
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;

        let name = req.name.clone().unwrap_or(existing.name);
        let color = req.color.clone().unwrap_or(existing.color);
        let sort_order = req.sort_order.unwrap_or(existing.sort_order);
        let hidden = req.hidden.unwrap_or(existing.hidden);

        sqlx::query_as!(
            ProjectStatus,
            r#"UPDATE project_statuses
               SET name = $2, color = $3, sort_order = $4, hidden = $5
               WHERE id = $1
               RETURNING id         as "id!: Uuid",
                         project_id as "project_id!: Uuid",
                         name,
                         color      as "color!",
                         sort_order as "sort_order!: i32",
                         hidden     as "hidden!: bool",
                         created_at as "created_at!: DateTime<Utc>""#,
            id,
            name,
            color,
            sort_order,
            hidden
        )
        .fetch_one(&mut *conn)
        .await
    }

    /// Partial update of a single status.
    pub async fn update(
        pool: &SqlitePool,
        id: Uuid,
        req: &UpdateProjectStatusRequest,
    ) -> Result<ProjectStatus, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        Self::update_on(&mut conn, id, req).await
    }

    /// Apply several column updates atomically (see `Issues::bulk_update`).
    pub async fn bulk_update(
        pool: &SqlitePool,
        updates: &[(Uuid, UpdateProjectStatusRequest)],
    ) -> Result<(), sqlx::Error> {
        let mut tx = pool.begin().await?;
        for (id, req) in updates {
            Self::update_on(&mut tx, *id, req).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete(pool: &SqlitePool, id: Uuid) -> Result<u64, sqlx::Error> {
        let result = sqlx::query!("DELETE FROM project_statuses WHERE id = $1", id)
            .execute(pool)
            .await?;
        Ok(result.rows_affected())
    }
}

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

/// SQLite storage row for an issue. Identical to `api_types::Issue` except
/// `extension_metadata` is TEXT here (parsed to JSON in `into_api`).
#[derive(Debug, Clone)]
struct IssueRow {
    id: Uuid,
    project_id: Uuid,
    issue_number: i32,
    simple_id: String,
    status_id: Uuid,
    title: String,
    description: Option<String>,
    priority: Option<IssuePriority>,
    start_date: Option<DateTime<Utc>>,
    target_date: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    sort_order: f64,
    parent_issue_id: Option<Uuid>,
    parent_issue_sort_order: Option<f64>,
    extension_metadata: String,
    creator_user_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl IssueRow {
    fn into_api(self) -> Issue {
        Issue {
            id: self.id,
            project_id: self.project_id,
            issue_number: self.issue_number,
            simple_id: self.simple_id,
            status_id: self.status_id,
            title: self.title,
            description: self.description,
            priority: self.priority,
            start_date: self.start_date,
            target_date: self.target_date,
            completed_at: self.completed_at,
            sort_order: self.sort_order,
            parent_issue_id: self.parent_issue_id,
            parent_issue_sort_order: self.parent_issue_sort_order,
            extension_metadata: serde_json::from_str(&self.extension_metadata).unwrap_or_else(
                |e| {
                    tracing::warn!(
                        issue_id = %self.id,
                        error = %e,
                        "corrupt extension_metadata TEXT; defaulting to empty object for this read"
                    );
                    serde_json::json!({})
                },
            ),
            creator_user_id: self.creator_user_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Namespace for `issues` CRUD in the `api_types::Issue` shape.
pub struct Issues;

impl Issues {
    async fn row_by_id<'e, E>(executor: E, id: Uuid) -> Result<Option<IssueRow>, sqlx::Error>
    where
        E: sqlx::SqliteExecutor<'e>,
    {
        sqlx::query_as!(
            IssueRow,
            r#"SELECT id                      as "id!: Uuid",
                      project_id              as "project_id!: Uuid",
                      issue_number            as "issue_number!: i32",
                      simple_id,
                      status_id               as "status_id!: Uuid",
                      title,
                      description,
                      priority                as "priority: IssuePriority",
                      start_date              as "start_date: DateTime<Utc>",
                      target_date             as "target_date: DateTime<Utc>",
                      completed_at            as "completed_at: DateTime<Utc>",
                      sort_order              as "sort_order!: f64",
                      parent_issue_id         as "parent_issue_id: Uuid",
                      parent_issue_sort_order as "parent_issue_sort_order: f64",
                      extension_metadata      as "extension_metadata!",
                      creator_user_id         as "creator_user_id: Uuid",
                      created_at              as "created_at!: DateTime<Utc>",
                      updated_at              as "updated_at!: DateTime<Utc>"
               FROM issues WHERE id = $1"#,
            id
        )
        .fetch_optional(executor)
        .await
    }

    async fn rows_by_project(
        pool: &SqlitePool,
        project_id: Uuid,
    ) -> Result<Vec<IssueRow>, sqlx::Error> {
        sqlx::query_as!(
            IssueRow,
            r#"SELECT id                      as "id!: Uuid",
                      project_id              as "project_id!: Uuid",
                      issue_number            as "issue_number!: i32",
                      simple_id,
                      status_id               as "status_id!: Uuid",
                      title,
                      description,
                      priority                as "priority: IssuePriority",
                      start_date              as "start_date: DateTime<Utc>",
                      target_date             as "target_date: DateTime<Utc>",
                      completed_at            as "completed_at: DateTime<Utc>",
                      sort_order              as "sort_order!: f64",
                      parent_issue_id         as "parent_issue_id: Uuid",
                      parent_issue_sort_order as "parent_issue_sort_order: f64",
                      extension_metadata      as "extension_metadata!",
                      creator_user_id         as "creator_user_id: Uuid",
                      created_at              as "created_at!: DateTime<Utc>",
                      updated_at              as "updated_at!: DateTime<Utc>"
               FROM issues
               WHERE project_id = $1
               ORDER BY sort_order ASC, created_at ASC"#,
            project_id
        )
        .fetch_all(pool)
        .await
    }

    pub async fn list_by_project(
        pool: &SqlitePool,
        project_id: Uuid,
    ) -> Result<ListIssuesResponse, sqlx::Error> {
        let issues: Vec<Issue> = Self::rows_by_project(pool, project_id)
            .await?
            .into_iter()
            .map(IssueRow::into_api)
            .collect();
        let total_count = issues.len();
        Ok(ListIssuesResponse {
            issues,
            total_count,
            limit: total_count,
            offset: 0,
        })
    }

    pub async fn get(pool: &SqlitePool, id: Uuid) -> Result<Option<Issue>, sqlx::Error> {
        Ok(Self::row_by_id(pool, id).await?.map(IssueRow::into_api))
    }

    /// Search within a project. Filters that matter for v1 are applied in Rust at
    /// local single-user scale; heavy filters can move into SQL later if needed.
    pub async fn search(
        pool: &SqlitePool,
        req: &SearchIssuesRequest,
    ) -> Result<ListIssuesResponse, sqlx::Error> {
        let all = Self::rows_by_project(pool, req.project_id).await?;
        let search = req.search.as_ref().map(|s| s.to_lowercase());

        let mut matched: Vec<Issue> = all
            .into_iter()
            .map(IssueRow::into_api)
            .filter(|i| {
                if let Some(status_id) = req.status_id {
                    if i.status_id != status_id {
                        return false;
                    }
                }
                if let Some(status_ids) = &req.status_ids {
                    if !status_ids.contains(&i.status_id) {
                        return false;
                    }
                }
                if let Some(priority) = req.priority {
                    if i.priority != Some(priority) {
                        return false;
                    }
                }
                if let Some(parent) = req.parent_issue_id {
                    if i.parent_issue_id != Some(parent) {
                        return false;
                    }
                }
                if let Some(simple_id) = &req.simple_id {
                    if &i.simple_id != simple_id {
                        return false;
                    }
                }
                if let Some(q) = &search {
                    let hay = format!(
                        "{} {}",
                        i.title.to_lowercase(),
                        i.description.as_deref().unwrap_or("").to_lowercase()
                    );
                    if !hay.contains(q) {
                        return false;
                    }
                }
                true
            })
            .collect();

        let total_count = matched.len();
        let offset = req.offset.unwrap_or(0).max(0) as usize;
        let limit = req.limit.map(|l| l.max(0) as usize).unwrap_or(total_count);
        let paged: Vec<Issue> = matched.drain(..).skip(offset).take(limit).collect();

        Ok(ListIssuesResponse {
            issues: paged,
            total_count,
            limit,
            offset,
        })
    }

    /// Create an issue, assigning `issue_number` + `simple_id` inside a single
    /// transaction (increment `projects.issue_counter`, join org for the prefix).
    /// The `UNIQUE(project_id, issue_number)` constraint is the backstop.
    pub async fn create(
        pool: &SqlitePool,
        req: &CreateIssueRequest,
        creator_user_id: Option<Uuid>,
    ) -> Result<Issue, sqlx::Error> {
        let id = req.id.unwrap_or_else(Uuid::new_v4);
        let extension_metadata = req.extension_metadata.to_string();

        let mut tx = pool.begin().await?;

        let counter = sqlx::query!(
            r#"UPDATE projects
               SET issue_counter = issue_counter + 1
               WHERE id = $1
               RETURNING issue_counter as "issue_counter!: i64",
                         organization_id as "organization_id!: Uuid""#,
            req.project_id
        )
        .fetch_one(&mut *tx)
        .await?;
        let issue_number = counter.issue_counter as i32;

        let prefix = sqlx::query_scalar!(
            r#"SELECT issue_prefix as "issue_prefix!" FROM organizations WHERE id = $1"#,
            counter.organization_id
        )
        .fetch_one(&mut *tx)
        .await?;
        let simple_id = format!("{prefix}-{issue_number}");

        let row = sqlx::query_as!(
            IssueRow,
            r#"INSERT INTO issues (
                   id, project_id, issue_number, simple_id, status_id, title, description,
                   priority, start_date, target_date, completed_at, sort_order,
                   parent_issue_id, parent_issue_sort_order, extension_metadata, creator_user_id
               )
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
               RETURNING id                      as "id!: Uuid",
                         project_id              as "project_id!: Uuid",
                         issue_number            as "issue_number!: i32",
                         simple_id,
                         status_id               as "status_id!: Uuid",
                         title,
                         description,
                         priority                as "priority: IssuePriority",
                         start_date              as "start_date: DateTime<Utc>",
                         target_date             as "target_date: DateTime<Utc>",
                         completed_at            as "completed_at: DateTime<Utc>",
                         sort_order              as "sort_order!: f64",
                         parent_issue_id         as "parent_issue_id: Uuid",
                         parent_issue_sort_order as "parent_issue_sort_order: f64",
                         extension_metadata      as "extension_metadata!",
                         creator_user_id         as "creator_user_id: Uuid",
                         created_at              as "created_at!: DateTime<Utc>",
                         updated_at              as "updated_at!: DateTime<Utc>""#,
            id,
            req.project_id,
            issue_number,
            simple_id,
            req.status_id,
            req.title,
            req.description,
            req.priority,
            req.start_date,
            req.target_date,
            req.completed_at,
            req.sort_order,
            req.parent_issue_id,
            req.parent_issue_sort_order,
            extension_metadata,
            creator_user_id
        )
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(row.into_api())
    }

    /// Partial update: fetch-then-overwrite (matches the `Tag` idiom) so the
    /// `Option<Option<T>>` "field present vs. absent" semantics stay explicit
    /// without dynamic SQL.
    /// Partial update on a caller-supplied connection, so `bulk_update` can run a
    /// whole batch inside one transaction (all-or-nothing).
    async fn update_on(
        conn: &mut sqlx::SqliteConnection,
        id: Uuid,
        req: &UpdateIssueRequest,
    ) -> Result<Issue, sqlx::Error> {
        let existing = Self::row_by_id(&mut *conn, id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?;

        let status_id = req.status_id.unwrap_or(existing.status_id);
        let title = req.title.clone().unwrap_or(existing.title);
        let description = match &req.description {
            Some(v) => v.clone(),
            None => existing.description,
        };
        let priority = match req.priority {
            Some(v) => v,
            None => existing.priority,
        };
        let start_date = match req.start_date {
            Some(v) => v,
            None => existing.start_date,
        };
        let target_date = match req.target_date {
            Some(v) => v,
            None => existing.target_date,
        };
        let completed_at = match req.completed_at {
            Some(v) => v,
            None => existing.completed_at,
        };
        let sort_order = req.sort_order.unwrap_or(existing.sort_order);
        let parent_issue_id = match req.parent_issue_id {
            Some(v) => v,
            None => existing.parent_issue_id,
        };
        let parent_issue_sort_order = match req.parent_issue_sort_order {
            Some(v) => v,
            None => existing.parent_issue_sort_order,
        };
        let extension_metadata = match &req.extension_metadata {
            Some(v) => v.to_string(),
            None => existing.extension_metadata,
        };

        let row = sqlx::query_as!(
            IssueRow,
            r#"UPDATE issues
               SET status_id = $2,
                   title = $3,
                   description = $4,
                   priority = $5,
                   start_date = $6,
                   target_date = $7,
                   completed_at = $8,
                   sort_order = $9,
                   parent_issue_id = $10,
                   parent_issue_sort_order = $11,
                   extension_metadata = $12,
                   updated_at = datetime('now', 'subsec')
               WHERE id = $1
               RETURNING id                      as "id!: Uuid",
                         project_id              as "project_id!: Uuid",
                         issue_number            as "issue_number!: i32",
                         simple_id,
                         status_id               as "status_id!: Uuid",
                         title,
                         description,
                         priority                as "priority: IssuePriority",
                         start_date              as "start_date: DateTime<Utc>",
                         target_date             as "target_date: DateTime<Utc>",
                         completed_at            as "completed_at: DateTime<Utc>",
                         sort_order              as "sort_order!: f64",
                         parent_issue_id         as "parent_issue_id: Uuid",
                         parent_issue_sort_order as "parent_issue_sort_order: f64",
                         extension_metadata      as "extension_metadata!",
                         creator_user_id         as "creator_user_id: Uuid",
                         created_at              as "created_at!: DateTime<Utc>",
                         updated_at              as "updated_at!: DateTime<Utc>""#,
            id,
            status_id,
            title,
            description,
            priority,
            start_date,
            target_date,
            completed_at,
            sort_order,
            parent_issue_id,
            parent_issue_sort_order,
            extension_metadata
        )
        .fetch_one(&mut *conn)
        .await?;

        // Outbound Linear mirror (JM-718): flag a *linked* card for a status
        // push only when the column actually changed. Scoped by
        // `linear_issue_id IS NOT NULL` in SQL so unlinked cards are never
        // flagged. This is the single choke point for both `update` and
        // `bulk_update` (both route through `update_on`) and the ONLY writer of
        // `linear_sync_pending = 1` — the sync loop's own writes use dedicated
        // methods that never set it (echo-loop invariant, ADR 0002 §8).
        if existing.status_id != status_id {
            sqlx::query!(
                r#"UPDATE issues
                   SET linear_sync_pending = 1
                   WHERE id = $1 AND linear_issue_id IS NOT NULL"#,
                id
            )
            .execute(&mut *conn)
            .await?;
        }

        Ok(row.into_api())
    }

    /// Partial update of a single issue.
    pub async fn update(
        pool: &SqlitePool,
        id: Uuid,
        req: &UpdateIssueRequest,
    ) -> Result<Issue, sqlx::Error> {
        let mut conn = pool.acquire().await?;
        Self::update_on(&mut conn, id, req).await
    }

    /// Apply several partial updates atomically. Drag-reorder sends the whole
    /// batch as one optimistic operation, so a mid-batch failure must not leave a
    /// partially reordered board — the transaction rolls back on the first error.
    pub async fn bulk_update(
        pool: &SqlitePool,
        updates: &[(Uuid, UpdateIssueRequest)],
    ) -> Result<(), sqlx::Error> {
        let mut tx = pool.begin().await?;
        for (id, req) in updates {
            Self::update_on(&mut tx, *id, req).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete(pool: &SqlitePool, id: Uuid) -> Result<u64, sqlx::Error> {
        let result = sqlx::query!("DELETE FROM issues WHERE id = $1", id)
            .execute(pool)
            .await?;
        Ok(result.rows_affected())
    }

    // --- Outbound Linear mirror (JM-718) -----------------------------------
    // These methods are the sync loop's exclusive write path plus one read
    // projection for the board badge. None routes through `update_on`, so none
    // can (re)set `linear_sync_pending = 1` — that flag is set only by the
    // user-driven mutation hook above (echo invariant).

    /// Read the Linear link projection for a project's linked cards (JM-718
    /// slice 5). A local-only board read — deliberately NOT folded onto the
    /// shared `api_types::Issue`, which the cloud `crates/remote` Postgres repo
    /// also uses (its schema has no `linear_*` columns). The badge merges this by
    /// `issue_id` on the frontend. Identifier/url are non-null because
    /// `link_linear` writes all link fields together.
    pub async fn list_linear_links(
        pool: &SqlitePool,
        project_id: Uuid,
    ) -> Result<Vec<LinearLinkRow>, sqlx::Error> {
        sqlx::query_as!(
            LinearLinkRow,
            r#"SELECT id                      as "issue_id!: Uuid",
                      linear_issue_identifier as "linear_issue_identifier!",
                      linear_url              as "linear_url!",
                      linear_sync_pending     as "linear_sync_pending!: i64"
               FROM issues
               WHERE project_id = $1 AND linear_issue_id IS NOT NULL"#,
            project_id
        )
        .fetch_all(pool)
        .await
    }

    /// Drain the cards flagged for an outbound Linear status push, joined with
    /// their project's bound account key. The `WHERE` guarantees every row has a
    /// `linear_issue_id`, so the field is non-null in the result.
    pub async fn list_pending_linear_sync(
        pool: &SqlitePool,
    ) -> Result<Vec<PendingLinearSync>, sqlx::Error> {
        sqlx::query_as!(
            PendingLinearSync,
            r#"SELECT i.id                 as "id!: Uuid",
                      i.status_id          as "status_id!: Uuid",
                      i.linear_issue_id    as "linear_issue_id!",
                      p.linear_account_key as "linear_account_key"
               FROM issues i
               JOIN projects p ON p.id = i.project_id
               WHERE i.linear_sync_pending = 1 AND i.linear_issue_id IS NOT NULL"#
        )
        .fetch_all(pool)
        .await
    }

    /// Record a successful push: clear the pending flag and remember the state we
    /// pushed (drift/idempotency baseline).
    ///
    /// `expected_status_id` is the column the loop *snapshotted and pushed*. The
    /// clear is conditional on the card still being in that column — guarding a
    /// TOCTOU race: if the user moves the card again while the Linear request is
    /// in flight, `update_on` re-sets `linear_sync_pending = 1` for the new
    /// column; an unconditional clear-by-id would wipe that newer flag and leave
    /// the board and Linear permanently diverged with no retry queued. When the
    /// column has changed the `UPDATE` matches zero rows, so the pending flag
    /// survives and the next tick re-syncs to the new column.
    pub async fn mark_linear_synced(
        pool: &SqlitePool,
        id: Uuid,
        expected_status_id: Uuid,
        state_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"UPDATE issues
               SET linear_sync_pending = 0, linear_state_id = $3
               WHERE id = $1 AND status_id = $2"#,
            id,
            expected_status_id,
            state_id
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Clear the pending flag without recording a state (skipped/unmapped card,
    /// or a deterministic failure we don't want to retry forever). Conditional on
    /// `expected_status_id` for the same anti-race reason as `mark_linear_synced`:
    /// if the card moved during the decision/push, leave the flag for the newer
    /// column rather than clearing it.
    pub async fn clear_linear_pending(
        pool: &SqlitePool,
        id: Uuid,
        expected_status_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"UPDATE issues SET linear_sync_pending = 0 WHERE id = $1 AND status_id = $2"#,
            id,
            expected_status_id
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Establish a manual card↔issue link (JM-718 slice 4). `linear_state_id`
    /// seeds the drift baseline (the issue's *current* Linear state at link
    /// time). `sync_pending` reconciles board-wins on link: if the card already
    /// sits in a mapped column whose target differs from the issue's current
    /// state, the caller passes `true` so the outbound loop pushes the board's
    /// state — otherwise the link would leave the board and Linear diverged with
    /// nothing queued (ADR 0002 board-wins). The partial `UNIQUE INDEX` on
    /// `linear_issue_id` is the backstop against linking one issue to two cards;
    /// a violation surfaces as a unique-constraint error the route maps to 409.
    pub async fn link_linear(
        pool: &SqlitePool,
        id: Uuid,
        linear_issue_id: &str,
        linear_issue_identifier: &str,
        linear_url: &str,
        linear_state_id: Option<&str>,
        sync_pending: bool,
    ) -> Result<(), sqlx::Error> {
        let pending = i64::from(sync_pending);
        sqlx::query!(
            r#"UPDATE issues
               SET linear_issue_id = $2,
                   linear_issue_identifier = $3,
                   linear_url = $4,
                   linear_state_id = $5,
                   linear_sync_pending = $6
               WHERE id = $1"#,
            id,
            linear_issue_id,
            linear_issue_identifier,
            linear_url,
            linear_state_id,
            pending
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Unlink a card whose Linear issue no longer exists (deleted in Linear).
    /// Clears every link field and the pending flag so it never retries a dead
    /// issue (ADR 0002 §7).
    pub async fn unlink_linear(pool: &SqlitePool, id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"UPDATE issues
               SET linear_issue_id = NULL,
                   linear_issue_identifier = NULL,
                   linear_url = NULL,
                   linear_state_id = NULL,
                   linear_sync_pending = 0
               WHERE id = $1"#,
            id
        )
        .execute(pool)
        .await?;
        Ok(())
    }

    // --- Inbound Linear import (JM-734) ------------------------------------

    /// Import Linear issues as new cards in `target_project_id`. Returns the
    /// number of cards actually inserted.
    ///
    /// Echo-loop safe: each card is written at `linear_sync_pending = 0` and this
    /// method NEVER routes through `update_on` (the sole `pending = 1` writer), so
    /// an imported card cannot appear in the next outbound drain (ADR 0002 §8
    /// invariant, extended to inbound).
    ///
    /// Dedup: the partial `UNIQUE(linear_issue_id)` index + `ON CONFLICT DO
    /// NOTHING` is the correctness backstop — idempotent across app restarts and
    /// across both accounts (Linear issue ids are globally unique). The batch
    /// pre-filter against already-linked ids is only a `projects.issue_counter`
    /// burn-avoidance optimization for the steady state (every issue already
    /// imported), NOT the dedup mechanism; a pre-filter/insert race can still
    /// no-op an insert after burning one counter value — negligible for a single
    /// poller and preferred over gapping numbers on every 60s tick.
    pub async fn import_from_linear(
        pool: &SqlitePool,
        target_project_id: Uuid,
        cards: &[ImportCard],
    ) -> Result<usize, sqlx::Error> {
        if cards.is_empty() {
            return Ok(0);
        }

        // Pre-filter: drop cards whose Linear issue is already linked to ANY card
        // (linear_issue_id is global). Avoids incrementing issue_counter for the
        // common all-already-imported poll. `json_each` gives an IN-list without
        // per-issue round-trips or dynamic SQL.
        let incoming_ids: Vec<&str> = cards.iter().map(|c| c.linear_issue_id.as_str()).collect();
        let ids_json = serde_json::to_string(&incoming_ids).unwrap_or_else(|_| "[]".to_string());
        let existing: std::collections::HashSet<String> = sqlx::query_scalar!(
            r#"SELECT linear_issue_id as "lid!"
               FROM issues
               WHERE linear_issue_id IN (SELECT value FROM json_each(?))"#,
            ids_json
        )
        .fetch_all(pool)
        .await?
        .into_iter()
        .collect();

        // Place imported cards after existing ones in the target project, in a
        // stable order (deterministic across a re-run of the same batch).
        let mut next_sort = sqlx::query_scalar!(
            r#"SELECT COALESCE(MAX(sort_order), -1.0) + 1.0 as "next!: f64"
               FROM issues WHERE project_id = $1"#,
            target_project_id
        )
        .fetch_one(pool)
        .await?;

        let mut inserted = 0usize;
        for card in cards {
            if existing.contains(&card.linear_issue_id) {
                continue;
            }
            let mut tx = pool.begin().await?;
            let counter = sqlx::query!(
                r#"UPDATE projects
                   SET issue_counter = issue_counter + 1
                   WHERE id = $1
                   RETURNING issue_counter as "issue_counter!: i64",
                             organization_id as "organization_id!: Uuid""#,
                target_project_id
            )
            .fetch_one(&mut *tx)
            .await?;
            let issue_number = counter.issue_counter as i32;
            let prefix = sqlx::query_scalar!(
                r#"SELECT issue_prefix as "issue_prefix!" FROM organizations WHERE id = $1"#,
                counter.organization_id
            )
            .fetch_one(&mut *tx)
            .await?;
            let simple_id = format!("{prefix}-{issue_number}");
            let id = Uuid::new_v4();

            let result = sqlx::query!(
                r#"INSERT INTO issues (
                       id, project_id, issue_number, simple_id, status_id, title,
                       sort_order, extension_metadata,
                       linear_issue_id, linear_issue_identifier, linear_url,
                       linear_state_id, linear_sync_pending
                   )
                   VALUES ($1, $2, $3, $4, $5, $6, $7, '{}', $8, $9, $10, $11, 0)
                   ON CONFLICT (linear_issue_id) WHERE linear_issue_id IS NOT NULL
                   DO NOTHING"#,
                id,
                target_project_id,
                issue_number,
                simple_id,
                card.status_id,
                card.title,
                next_sort,
                card.linear_issue_id,
                card.linear_issue_identifier,
                card.linear_url,
                card.linear_state_id,
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;

            if result.rows_affected() > 0 {
                inserted += 1;
                next_sort += 1.0;
            }
        }
        Ok(inserted)
    }
}

/// A pre-resolved inbound card ready to insert (JM-734). The caller (the sync
/// loop's inbound sweep) resolves `status_id` via [`resolve_import_status`]
/// before handing the batch to [`Issues::import_from_linear`]; this keeps the DB
/// crate free of any dependency on the Linear client's DTOs.
#[derive(Debug, Clone)]
pub struct ImportCard {
    pub linear_issue_id: String,
    pub linear_issue_identifier: String,
    pub linear_url: String,
    pub title: String,
    pub linear_state_id: Option<String>,
    /// The resolved local board column (from `resolve_import_status`).
    pub status_id: Uuid,
}

/// Resolve which local board column an inbound Linear issue lands in.
///
/// `statuses` = the import-target project's columns. `state_map` = the account's
/// OUTBOUND map (`project_status_id -> linear_state_id`), which may span EVERY
/// project bound to the account. We invert ONLY the entries whose
/// `project_status_id` belongs to `statuses` — a blind inversion of the whole
/// map can resolve to a status in a different project, and since the FK checks
/// existence (not project membership) the card would insert but render in no
/// column. Within-target collision (two of this project's columns map to one
/// Linear state) is resolved by lowest `sort_order` (leftmost) deterministically.
///
/// Falls back to the leftmost non-hidden column (intake) when the incoming state
/// has no scoped mapping. Returns `None` only when the project has no non-hidden
/// column at all — the caller must then skip the issue (status_id is NOT NULL).
pub fn resolve_import_status(
    statuses: &[ProjectStatus],
    state_map: &std::collections::HashMap<String, String>,
    incoming_state_id: Option<&str>,
) -> Option<Uuid> {
    if let Some(state) = incoming_state_id {
        let mut matched: Vec<&ProjectStatus> = statuses
            .iter()
            .filter(|s| {
                !s.hidden && state_map.get(&s.id.to_string()).map(String::as_str) == Some(state)
            })
            .collect();
        if !matched.is_empty() {
            matched.sort_by_key(|s| (s.sort_order, s.created_at));
            return Some(matched[0].id);
        }
    }
    statuses
        .iter()
        .filter(|s| !s.hidden)
        .min_by_key(|s| (s.sort_order, s.created_at))
        .map(|s| s.id)
}

// ---------------------------------------------------------------------------
// Pull requests (board fallback shapes — JM-749)
// ---------------------------------------------------------------------------

/// One local PR joined to the board issue it belongs to (through its workspace).
/// Internal query shape only; mapped into the two serde row types below.
struct PrJoinRow {
    id: String,
    pr_url: String,
    pr_number: i64,
    pr_status: MergeStatus,
    merged_at: Option<DateTime<Utc>>,
    merge_commit_sha: Option<String>,
    target_branch_name: String,
    project_id: Uuid,
    issue_id: Uuid,
    workspace_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    check_status: Option<CheckStatus>,
}

/// A local PR shaped for the board's `pull_requests` Electric fallback snapshot.
/// Columns match `shared/remote-types.ts` `PullRequest` EXACTLY, plus ONE extra:
/// `check_status` (JM-749).
///
/// The `check_status` field is NOT declared on the generated Electric
/// `PullRequest` type. It survives to the card because the local fallback
/// collection is created with no schema to strip unknown keys (see
/// `packages/web-core/src/shared/lib/electric/collections.ts`
/// `createShapeCollection` — no `schema` arg → TanStack DB stores rows verbatim).
/// READ SITE: `KanbanContainer.tsx` issue-level PR mapping reads `pr.check_status`
/// and feeds `PrChecksBadge`. If a future rebase attaches a Standard Schema to
/// these collections, this field vanishes silently — keep emit + read in sync.
#[derive(Debug, Serialize)]
pub struct BoardPullRequestRow {
    pub id: String,
    pub url: String,
    pub number: i64,
    pub status: &'static str,
    pub merged_at: Option<DateTime<Utc>>,
    pub merge_commit_sha: Option<String>,
    pub target_branch_name: String,
    pub project_id: Uuid,
    pub issue_id: Uuid,
    pub workspace_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub check_status: Option<CheckStatus>,
}

/// A synthesized `pull_request_issues` join row (JM-749). No local table exists;
/// the frontend's `getPullRequestsForIssue` matches PRs to an issue THROUGH this
/// shape, so it must be populated for a PR to appear on a card. `id ==
/// pull_request_id == pull_requests.id`: each local PR maps to exactly one issue
/// (via its single workspace), so `pr.id` is a collision-proof key. NEVER derive
/// `id` from `issue_id` — an issue with two PRs would collide and one PR would
/// vanish from the card.
#[derive(Debug, Serialize)]
pub struct BoardPullRequestIssueRow {
    pub id: String,
    pub pull_request_id: String,
    pub issue_id: Uuid,
}

/// Namespace for reading local pull requests in the board fallback shapes.
pub struct BoardPullRequests;

impl BoardPullRequests {
    /// Every local PR whose workspace is linked to an issue in `project_id`,
    /// shaped for both the `pull_requests` and `pull_request_issues` board
    /// fallbacks. Scoping walks `pull_requests → workspaces → issues.project_id`;
    /// the INNER JOINs exclude PRs on ad-hoc (issue-less) workspaces.
    pub async fn list_by_project(
        pool: &SqlitePool,
        project_id: Uuid,
    ) -> Result<(Vec<BoardPullRequestRow>, Vec<BoardPullRequestIssueRow>), sqlx::Error> {
        let rows = sqlx::query_as!(
            PrJoinRow,
            r#"SELECT pr.id                 as "id!",
                      pr.pr_url             as "pr_url!",
                      pr.pr_number          as "pr_number!: i64",
                      pr.pr_status          as "pr_status!: MergeStatus",
                      pr.merged_at          as "merged_at: DateTime<Utc>",
                      pr.merge_commit_sha,
                      pr.target_branch_name as "target_branch_name!",
                      i.project_id          as "project_id!: Uuid",
                      w.issue_id            as "issue_id!: Uuid",
                      pr.workspace_id       as "workspace_id: Uuid",
                      pr.created_at         as "created_at!: DateTime<Utc>",
                      pr.updated_at         as "updated_at!: DateTime<Utc>",
                      pr.check_status       as "check_status: CheckStatus"
               FROM pull_requests pr
               JOIN workspaces w ON pr.workspace_id = w.id
               JOIN issues i     ON w.issue_id = i.id
               WHERE i.project_id = $1"#,
            project_id
        )
        .fetch_all(pool)
        .await?;

        let mut prs = Vec::with_capacity(rows.len());
        let mut links = Vec::with_capacity(rows.len());
        for r in rows {
            links.push(BoardPullRequestIssueRow {
                id: r.id.clone(),
                pull_request_id: r.id.clone(),
                issue_id: r.issue_id,
            });
            prs.push(BoardPullRequestRow {
                id: r.id,
                url: r.pr_url,
                number: r.pr_number,
                // `Unknown` is not a valid Electric PullRequestStatus
                // (open|merged|closed); collapse it to `closed` so it can't leak
                // through as a green "open" PR — PrBadge treats any unrecognized
                // value as open.
                status: match r.pr_status {
                    MergeStatus::Open => "open",
                    MergeStatus::Merged => "merged",
                    MergeStatus::Closed | MergeStatus::Unknown => "closed",
                },
                merged_at: r.merged_at,
                merge_commit_sha: r.merge_commit_sha,
                target_branch_name: r.target_branch_name,
                project_id: r.project_id,
                issue_id: r.issue_id,
                workspace_id: r.workspace_id,
                created_at: r.created_at,
                updated_at: r.updated_at,
                check_status: r.check_status,
            });
        }
        Ok((prs, links))
    }
}

// ---------------------------------------------------------------------------
// Workspaces (board fallback shape — JM-751)
// ---------------------------------------------------------------------------

/// One local workspace joined to the board issue it belongs to. Internal query
/// shape only; mapped into `BoardWorkspaceRow` (which injects the local-mode
/// sentinel/None fields the `workspaces` table doesn't carry).
struct WsJoinRow {
    id: Uuid,
    project_id: Uuid,
    issue_id: Uuid,
    name: Option<String>,
    archived: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

/// A local workspace shaped for the board's `project_workspaces` Electric
/// fallback snapshot (JM-751). Columns match `shared/remote-types.ts` `Workspace`
/// EXACTLY — no extra fields, so unlike `BoardPullRequestRow` there is no
/// schema-strip hazard to guard.
///
/// Local-mode field collapse: there is no remote/local workspace split, so `id`
/// and `local_workspace_id` are BOTH the local `Workspace.id`. That single value
/// satisfies two independent client-side joins:
///   - `pr.workspace_id == workspace.id` (PR → workspace card, `prsByWorkspaceId`
///     in `KanbanContainer.tsx`), and
///   - `workspace.local_workspace_id ==` a live sidebar-stream key
///     (`localWorkspacesById`, keyed by local workspace id) → the client merges in
///     `branch` + `runningAgents` from the already-served `/api/workspaces`
///     stream. Those two runtime fields are therefore deliberately NOT emitted
///     here — emitting them would be dead weight the card ignores.
///
/// `owner_user_id` is the `LOCAL_USER_ID` sentinel. Diff stats
/// (`files_changed`/`lines_added`/`lines_removed`) are `None` — the local
/// `workspaces` table has no diff columns, so the card shows "0 changed" (a known
/// cosmetic gap; the branch/agent chips, the JM-751 goal, are unaffected).
#[derive(Debug, Serialize)]
pub struct BoardWorkspaceRow {
    pub id: Uuid,
    pub project_id: Uuid,
    pub owner_user_id: Uuid,
    pub issue_id: Uuid,
    pub local_workspace_id: Uuid,
    pub name: Option<String>,
    pub archived: bool,
    pub files_changed: Option<i64>,
    pub lines_added: Option<i64>,
    pub lines_removed: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Namespace for reading local workspaces in the board fallback shape.
pub struct BoardWorkspaces;

impl BoardWorkspaces {
    /// Every local workspace linked to an issue in `project_id`, shaped for the
    /// `project_workspaces` board fallback. Scoping walks `workspaces →
    /// issues.project_id` through the JM-749 `workspaces.issue_id` link; the INNER
    /// JOIN excludes ad-hoc (issue-less) workspaces and guarantees `issue_id` is
    /// non-null in the result (a NULL `issue_id` matches no `issues.id`). The
    /// frontend further filters to non-archived workspaces that have a live
    /// sidebar-stream entry, so archived rows are emitted verbatim and dropped
    /// client-side.
    pub async fn list_by_project(
        pool: &SqlitePool,
        project_id: Uuid,
    ) -> Result<Vec<BoardWorkspaceRow>, sqlx::Error> {
        let rows = sqlx::query_as!(
            WsJoinRow,
            r#"SELECT w.id         as "id!: Uuid",
                      i.project_id as "project_id!: Uuid",
                      w.issue_id   as "issue_id!: Uuid",
                      w.name,
                      w.archived   as "archived!: bool",
                      w.created_at as "created_at!: DateTime<Utc>",
                      w.updated_at as "updated_at!: DateTime<Utc>"
               FROM workspaces w
               JOIN issues i ON w.issue_id = i.id
               WHERE i.project_id = $1"#,
            project_id
        )
        .fetch_all(pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| BoardWorkspaceRow {
                id: r.id,
                project_id: r.project_id,
                owner_user_id: crate::LOCAL_USER_ID,
                issue_id: r.issue_id,
                // Local mode: the workspace IS the local workspace, so both the
                // PR-link key (`id`) and the sidebar-merge key
                // (`local_workspace_id`) are the same UUID.
                local_workspace_id: r.id,
                name: r.name,
                archived: r.archived,
                files_changed: None,
                lines_added: None,
                lines_removed: None,
                created_at: r.created_at,
                updated_at: r.updated_at,
            })
            .collect())
    }
}

#[cfg(test)]
mod import_tests {
    use std::collections::HashMap;

    use api_types::{CreateIssueRequest, CreateProjectRequest, ProjectStatus};
    use chrono::{TimeZone, Utc};
    use sqlx::SqlitePool;
    use uuid::Uuid;

    use super::{
        BoardProjects, BoardPullRequests, BoardWorkspaces, ImportCard, Issues, ProjectStatuses,
        resolve_import_status,
    };
    use crate::models::{
        merge::{CheckStatus, MergeStatus},
        pull_request::PullRequest,
        workspace::{CreateWorkspace, Workspace},
    };

    fn status(sort_order: i32, hidden: bool) -> ProjectStatus {
        ProjectStatus {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            name: "col".into(),
            color: "#fff".into(),
            sort_order,
            hidden,
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
        }
    }

    // --- pure resolve_import_status (covers the design-review findings) ------

    #[test]
    fn resolve_prefers_scoped_mapped_status() {
        let s0 = status(0, false);
        let s1 = status(1, false);
        let map = HashMap::from([(s1.id.to_string(), "linear-started".to_string())]);
        let got = resolve_import_status(&[s0.clone(), s1.clone()], &map, Some("linear-started"));
        assert_eq!(got, Some(s1.id));
    }

    #[test]
    fn resolve_excludes_cross_project_map_entries() {
        // The account map contains a mapping for a status that is NOT in this
        // project (a sibling project's column) pointing at the incoming state.
        // It must be ignored — else we'd insert a foreign-project status_id.
        let mine = status(0, false);
        let foreign_id = Uuid::new_v4();
        let map = HashMap::from([(foreign_id.to_string(), "linear-started".to_string())]);
        let got = resolve_import_status(&[mine.clone()], &map, Some("linear-started"));
        // No scoped mapping → falls back to the leftmost column of THIS project.
        assert_eq!(got, Some(mine.id));
    }

    #[test]
    fn resolve_collision_picks_lowest_sort_order() {
        let leftmost = status(0, false);
        let rightmost = status(5, false);
        let map = HashMap::from([
            (leftmost.id.to_string(), "S".to_string()),
            (rightmost.id.to_string(), "S".to_string()),
        ]);
        // Deliberately pass rightmost first to prove ordering, not input order.
        let got = resolve_import_status(&[rightmost.clone(), leftmost.clone()], &map, Some("S"));
        assert_eq!(got, Some(leftmost.id));
    }

    #[test]
    fn resolve_mapped_hidden_falls_back_to_visible() {
        // A hidden column mapped to the incoming state must NOT swallow the card;
        // the mapped branch skips it and falls through to the leftmost visible.
        let hidden_mapped = status(0, true);
        let visible = status(1, false);
        let map = HashMap::from([(hidden_mapped.id.to_string(), "S".to_string())]);
        let got = resolve_import_status(&[hidden_mapped, visible.clone()], &map, Some("S"));
        assert_eq!(got, Some(visible.id));
    }

    #[test]
    fn resolve_falls_back_to_leftmost_non_hidden() {
        let hidden = status(0, true);
        let first_visible = status(1, false);
        let later = status(2, false);
        let got = resolve_import_status(
            &[hidden, first_visible.clone(), later],
            &HashMap::new(),
            Some("unmapped-state"),
        );
        assert_eq!(got, Some(first_visible.id));
    }

    #[test]
    fn resolve_none_when_no_visible_column() {
        let only_hidden = status(0, true);
        assert_eq!(
            resolve_import_status(&[only_hidden], &HashMap::new(), None),
            None
        );
        assert_eq!(resolve_import_status(&[], &HashMap::new(), Some("x")), None);
    }

    // --- import_from_linear (echo-loop + dedup, AC "test this") --------------

    async fn mem_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    fn card(linear_id: &str, status_id: Uuid) -> ImportCard {
        ImportCard {
            linear_issue_id: linear_id.into(),
            linear_issue_identifier: format!("JM-{linear_id}"),
            linear_url: format!("https://linear.app/x/{linear_id}"),
            title: format!("Imported {linear_id}"),
            linear_state_id: Some("linear-started".into()),
            status_id,
        }
    }

    #[tokio::test]
    async fn import_inserts_dedups_and_stays_out_of_outbound_drain() {
        let pool = mem_pool().await;
        let project = BoardProjects::create(
            &pool,
            &CreateProjectRequest {
                id: None,
                organization_id: crate::LOCAL_ORG_ID,
                name: "Board".into(),
                color: "#abc".into(),
            },
        )
        .await
        .unwrap();
        // Bind the project to an account — proves echo-loop safety even when the
        // outbound drain WOULD otherwise consider this project's linked cards.
        BoardProjects::set_linear_account_key(&pool, project.id, Some("work".into()))
            .await
            .unwrap();
        let statuses = ProjectStatuses::list_by_project(&pool, project.id)
            .await
            .unwrap();
        let intake = statuses[0].id;

        let batch = vec![card("A", intake), card("B", intake)];
        let n = Issues::import_from_linear(&pool, project.id, &batch)
            .await
            .unwrap();
        assert_eq!(n, 2, "both new issues imported");

        // Re-import the same batch → zero new (partial-unique dedup), no error.
        let n2 = Issues::import_from_linear(&pool, project.id, &batch)
            .await
            .unwrap();
        assert_eq!(n2, 0, "already-imported issues are skipped");

        // Echo-loop invariant: imported cards are written at pending=0 and never
        // route through update_on, so the outbound drain sees nothing.
        let pending = Issues::list_pending_linear_sync(&pool).await.unwrap();
        assert!(
            pending.is_empty(),
            "imported cards must not enter the outbound drain"
        );

        // And they are real, linked cards on the board.
        let links = Issues::list_linear_links(&pool, project.id).await.unwrap();
        assert_eq!(links.len(), 2);
        assert!(links.iter().all(|l| l.linear_sync_pending == 0));
    }

    // --- BoardPullRequests::list_by_project (JM-749) -------------------------

    /// Seed a board project with one issue, then return (pool, project_id,
    /// issue_id) ready for workspace/PR attachment.
    async fn project_with_issue() -> (SqlitePool, Uuid, Uuid) {
        let pool = mem_pool().await;
        let project = BoardProjects::create(
            &pool,
            &CreateProjectRequest {
                id: None,
                organization_id: crate::LOCAL_ORG_ID,
                name: "Board".into(),
                color: "#abc".into(),
            },
        )
        .await
        .unwrap();
        let status_id = ProjectStatuses::list_by_project(&pool, project.id)
            .await
            .unwrap()[0]
            .id;
        let issue = Issues::create(
            &pool,
            &CreateIssueRequest {
                id: None,
                project_id: project.id,
                status_id,
                title: "I1".into(),
                description: None,
                priority: None,
                start_date: None,
                target_date: None,
                completed_at: None,
                sort_order: 0.0,
                parent_issue_id: None,
                parent_issue_sort_order: None,
                extension_metadata: serde_json::json!({}),
            },
            None,
        )
        .await
        .unwrap();
        (pool, project.id, issue.id)
    }

    #[tokio::test]
    async fn board_prs_scope_to_issue_linked_workspaces_and_carry_check_status() {
        let (pool, project_id, issue_id) = project_with_issue().await;

        // Issue-linked workspace + open PR with a passing check.
        let ws = Workspace::create(
            &pool,
            &CreateWorkspace {
                branch: "b1".into(),
                name: None,
            },
            Uuid::new_v4(),
        )
        .await
        .unwrap();
        Workspace::set_issue_id(&pool, ws.id, issue_id)
            .await
            .unwrap();
        let pr = PullRequest::create(&pool, Some(ws.id), None, "https://x/1", 1, "main")
            .await
            .unwrap();
        PullRequest::update_check_status(&pool, &pr.pr_url, Some(CheckStatus::Passing))
            .await
            .unwrap();

        // Ad-hoc workspace (no issue link) + its PR — MUST be excluded by the
        // INNER JOIN through issues.project_id.
        let adhoc = Workspace::create(
            &pool,
            &CreateWorkspace {
                branch: "b2".into(),
                name: None,
            },
            Uuid::new_v4(),
        )
        .await
        .unwrap();
        PullRequest::create(&pool, Some(adhoc.id), None, "https://x/2", 2, "main")
            .await
            .unwrap();

        let (prs, links) = BoardPullRequests::list_by_project(&pool, project_id)
            .await
            .unwrap();

        assert_eq!(prs.len(), 1, "only the issue-linked PR is in scope");
        let row = &prs[0];
        assert_eq!(row.id, pr.id);
        assert_eq!(row.issue_id, issue_id);
        assert_eq!(row.project_id, project_id);
        assert_eq!(row.workspace_id, Some(ws.id));
        assert_eq!(row.status, "open");
        assert_eq!(
            row.check_status,
            Some(CheckStatus::Passing),
            "check status passes through to the fallback row"
        );

        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].id, pr.id,
            "link id == pull_request_id == pr.id (collision-proof synthetic key)"
        );
        assert_eq!(links[0].pull_request_id, pr.id);
        assert_eq!(links[0].issue_id, issue_id);
    }

    #[tokio::test]
    async fn board_prs_collapse_unknown_status_to_closed() {
        let (pool, project_id, issue_id) = project_with_issue().await;
        let ws = Workspace::create(
            &pool,
            &CreateWorkspace {
                branch: "b1".into(),
                name: None,
            },
            Uuid::new_v4(),
        )
        .await
        .unwrap();
        Workspace::set_issue_id(&pool, ws.id, issue_id)
            .await
            .unwrap();
        let pr = PullRequest::create(&pool, Some(ws.id), None, "https://x/1", 1, "main")
            .await
            .unwrap();
        PullRequest::update_status(&pool, &pr.pr_url, &MergeStatus::Unknown, None, None)
            .await
            .unwrap();

        let (prs, _links) = BoardPullRequests::list_by_project(&pool, project_id)
            .await
            .unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(
            prs[0].status, "closed",
            "Unknown must collapse to closed, never leak as a green open PR"
        );
    }

    // --- BoardWorkspaces::list_by_project (JM-751) ---------------------------

    #[tokio::test]
    async fn board_workspaces_scope_to_issue_and_collapse_local_ids() {
        let (pool, project_id, issue_id) = project_with_issue().await;

        // Issue-linked workspace — in scope.
        let ws = Workspace::create(
            &pool,
            &CreateWorkspace {
                branch: "feat/x".into(),
                name: Some("W1".into()),
            },
            Uuid::new_v4(),
        )
        .await
        .unwrap();
        Workspace::set_issue_id(&pool, ws.id, issue_id)
            .await
            .unwrap();

        // Ad-hoc workspace (no issue link) — MUST be excluded by the INNER JOIN.
        Workspace::create(
            &pool,
            &CreateWorkspace {
                branch: "feat/adhoc".into(),
                name: None,
            },
            Uuid::new_v4(),
        )
        .await
        .unwrap();

        let rows = BoardWorkspaces::list_by_project(&pool, project_id)
            .await
            .unwrap();

        assert_eq!(rows.len(), 1, "only the issue-linked workspace is in scope");
        let row = &rows[0];
        assert_eq!(row.id, ws.id);
        assert_eq!(row.issue_id, issue_id);
        assert_eq!(row.project_id, project_id);
        assert_eq!(row.name.as_deref(), Some("W1"));
        assert!(!row.archived);
        // Local-mode collapse: both client-join keys resolve to the workspace id.
        assert_eq!(
            row.local_workspace_id, ws.id,
            "local_workspace_id must equal id so the sidebar-stream merge (branch/agents) resolves"
        );
        assert_eq!(
            row.owner_user_id,
            crate::LOCAL_USER_ID,
            "owner is the local sentinel"
        );
    }
}

/// One linked card's Linear projection for the board badge. Produced by
/// [`Issues::list_linear_links`].
#[derive(Debug, Clone)]
pub struct LinearLinkRow {
    pub issue_id: Uuid,
    pub linear_issue_identifier: String,
    pub linear_url: String,
    /// `1` while a status change is queued for an outbound push.
    pub linear_sync_pending: i64,
}

/// A card flagged for an outbound Linear status push, joined with its project's
/// bound account key. Produced by [`Issues::list_pending_linear_sync`].
#[derive(Debug, Clone)]
pub struct PendingLinearSync {
    pub id: Uuid,
    pub status_id: Uuid,
    /// Non-null: the drain query filters `linear_issue_id IS NOT NULL`.
    pub linear_issue_id: String,
    /// The account key bound to the card's project (`projects.linear_account_key`).
    /// `None` means the project isn't bound to a Linear account yet.
    pub linear_account_key: Option<String>,
}
