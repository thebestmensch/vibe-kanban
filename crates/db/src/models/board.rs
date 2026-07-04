//! Local-SQLite persistence for the kanban board (JM-714 v1).
//!
//! Re-backs the board entities that used to live in the cloud (`crates/remote`)
//! against the local SQLite pool, producing the exact `api_types` DTOs so the
//! `/api/remote/*` routes keep their contract without an Electric/Postgres round
//! trip. Storage diverges from the DTOs only where SQLite forces it (JSON stored
//! as TEXT, timestamps as TEXT, UUIDs as BLOB); `IssueRow::into_api` bridges the
//! one lossy field (`extension_metadata`).

use api_types::{
    CreateIssueRequest, CreateProjectStatusRequest, Issue, IssuePriority, ListIssuesResponse,
    Project, ProjectStatus, SearchIssuesRequest, UpdateIssueRequest, UpdateProjectStatusRequest,
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

    pub async fn update(
        pool: &SqlitePool,
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
        .fetch_optional(pool)
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
        .fetch_one(pool)
        .await
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
            extension_metadata: serde_json::from_str(&self.extension_metadata)
                .unwrap_or_else(|_| serde_json::json!({})),
            creator_user_id: self.creator_user_id,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Namespace for `issues` CRUD in the `api_types::Issue` shape.
pub struct Issues;

impl Issues {
    async fn row_by_id(pool: &SqlitePool, id: Uuid) -> Result<Option<IssueRow>, sqlx::Error> {
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
        .fetch_optional(pool)
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
    pub async fn update(
        pool: &SqlitePool,
        id: Uuid,
        req: &UpdateIssueRequest,
    ) -> Result<Issue, sqlx::Error> {
        let existing = Self::row_by_id(pool, id)
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
        .fetch_one(pool)
        .await?;

        Ok(row.into_api())
    }

    pub async fn delete(pool: &SqlitePool, id: Uuid) -> Result<u64, sqlx::Error> {
        let result = sqlx::query!("DELETE FROM issues WHERE id = $1", id)
            .execute(pool)
            .await?;
        Ok(result.rows_affected())
    }
}
