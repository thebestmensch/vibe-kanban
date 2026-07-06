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
use sqlx::SqlitePool;
use uuid::Uuid;

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
