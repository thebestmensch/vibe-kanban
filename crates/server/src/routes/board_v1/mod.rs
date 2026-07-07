//! Local board API (JM-714 v1).
//!
//! The frontend's Electric/REST sync layer (`packages/web-core/src/shared/lib/
//! electric/collections.ts`) reads board data from `/v1/fallback/{table}` and
//! expects a raw snapshot body shaped `{ "<table>": [ ...rows ] }` (see
//! `extractFallbackRows`). This differs from the `ApiResponse`-wrapped shape the
//! `/api/remote/*` routes serve — those feed the org/project chooser chain, not
//! the board's per-shape sync. This router serves the snapshot shape directly,
//! re-using the SQLite-backed `board` models.
//!
//! Mounted at origin root (sibling to `/api`) with no relay-signature/auth
//! middleware: these are same-origin local reads, and the fetch wrapper's bearer
//! token is a fixed local sentinel the backend ignores.

use api_types::{ListMembersResponse, ListOrganizationsResponse, MemberRole, OrganizationWithRole};
use axum::{
    Router,
    extract::{Path, Query, State},
    response::Json as ResponseJson,
    routing::get,
};
use chrono::Utc;
use db::{
    LOCAL_ORG_ID,
    models::board::{BoardProjects, BoardPullRequests, BoardWorkspaces, Issues, ProjectStatuses},
};
use deployment::Deployment;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

mod mutations;

pub fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/organizations", get(list_organizations))
        .route("/organizations/{org_id}/members", get(list_members))
        .route("/fallback/issues", get(fallback_issues))
        .route("/fallback/project_statuses", get(fallback_project_statuses))
        .route("/fallback/projects", get(fallback_projects))
        .route("/fallback/pull_requests", get(fallback_pull_requests))
        .route(
            "/fallback/pull_request_issues",
            get(fallback_pull_request_issues),
        )
        .route(
            "/fallback/project_workspaces",
            get(fallback_project_workspaces),
        )
        .route("/fallback/{table}", get(fallback_stub))
        .merge(mutations::router())
}

/// The single seeded local organization. Synthesized (not read from the row)
/// because the board only needs a consistent `id` (matching the seeded
/// `projects.organization_id` FK) to cascade org-select → project load; the
/// remaining fields are cosmetic. Served raw (not `ApiResponse`-wrapped): the
/// frontend's `handleRemoteResponse` reads the body directly.
async fn list_organizations() -> ResponseJson<ListOrganizationsResponse> {
    let now = Utc::now();
    ResponseJson(ListOrganizationsResponse {
        organizations: vec![OrganizationWithRole {
            id: LOCAL_ORG_ID,
            name: "Local Board".to_string(),
            slug: "local".to_string(),
            is_personal: true,
            issue_prefix: "VK".to_string(),
            created_at: now,
            updated_at: now,
            user_role: MemberRole::Admin,
        }],
    })
}

#[derive(Debug, Deserialize)]
struct ProjectScoped {
    project_id: Uuid,
}

#[derive(Debug, Deserialize)]
struct OrgScoped {
    organization_id: Uuid,
}

/// Build the `{ "<table>": [ ...rows ] }` snapshot body the fallback sync expects.
fn snapshot<T: serde::Serialize>(table: &str, rows: T) -> ResponseJson<Value> {
    let mut map = serde_json::Map::new();
    map.insert(table.to_string(), json!(rows));
    ResponseJson(Value::Object(map))
}

/// Org membership is a cloud concept with no local model. The board only reads
/// this to build a lookup map (empty is fine), so stub it empty to keep
/// `OrgProvider`'s members query from 404-ing.
async fn list_members(Path(_org_id): Path<Uuid>) -> ResponseJson<ListMembersResponse> {
    ResponseJson(ListMembersResponse {
        members: Vec::new(),
    })
}

async fn fallback_issues(
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<ProjectScoped>,
) -> Result<ResponseJson<Value>, ApiError> {
    let response = Issues::list_by_project(&deployment.db().pool, query.project_id).await?;
    Ok(snapshot("issues", response.issues))
}

async fn fallback_project_statuses(
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<ProjectScoped>,
) -> Result<ResponseJson<Value>, ApiError> {
    let rows = ProjectStatuses::list_by_project(&deployment.db().pool, query.project_id).await?;
    Ok(snapshot("project_statuses", rows))
}

async fn fallback_projects(
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<OrgScoped>,
) -> Result<ResponseJson<Value>, ApiError> {
    let rows = BoardProjects::list_by_org(&deployment.db().pool, query.organization_id).await?;
    Ok(snapshot("projects", rows))
}

/// JM-749: local PRs shaped for the board card's `pull_requests` fallback, joined
/// issue → workspace → PR. Carries an extra `check_status` field (2a) that feeds
/// the card's CI-check badge; see `BoardPullRequestRow`. The sibling
/// `pull_request_issues` route below provides the issue↔PR join the frontend uses
/// to attach these to a card. Each is fetched independently by the sync layer, so
/// each route runs its own query (30s refresh cadence — a fresh PR/check shows on
/// the next tick, not instantly).
async fn fallback_pull_requests(
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<ProjectScoped>,
) -> Result<ResponseJson<Value>, ApiError> {
    let (prs, _links) =
        BoardPullRequests::list_by_project(&deployment.db().pool, query.project_id).await?;
    Ok(snapshot("pull_requests", prs))
}

async fn fallback_pull_request_issues(
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<ProjectScoped>,
) -> Result<ResponseJson<Value>, ApiError> {
    let (_prs, links) =
        BoardPullRequests::list_by_project(&deployment.db().pool, query.project_id).await?;
    Ok(snapshot("pull_request_issues", links))
}

/// JM-751: local workspaces shaped for the board card's `project_workspaces`
/// fallback. The snapshot key is `workspaces` (the shape's `table` string), which
/// diverges from the `project_workspaces` URL segment — cf. the stub's
/// segment→table remap below. Joins issue → workspace via the JM-749
/// `workspaces.issue_id` link. `branch` + running-agent chips are NOT in this
/// snapshot: the card enriches them client-side from the already-served
/// `/api/workspaces` sidebar stream, keyed by `local_workspace_id` (== the row
/// `id` in local mode). PRs attached to these workspaces render under the
/// workspace sub-card via `pull_requests.workspace_id == workspace.id`.
async fn fallback_project_workspaces(
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<ProjectScoped>,
) -> Result<ResponseJson<Value>, ApiError> {
    let rows = BoardWorkspaces::list_by_project(&deployment.db().pool, query.project_id).await?;
    Ok(snapshot("workspaces", rows))
}

/// Empty snapshot for every board-adjacent shape without a local backend yet
/// (comments, reactions, tags, assignees, relationships, …).
/// These are non-blocking for board render; returning an empty array with the
/// correct table key keeps `extractFallbackRows` happy so no sync error fires.
///
/// The response key must equal the shape's `table` string, which diverges from
/// the URL segment for a few shapes (see `shared/remote-types.ts`).
async fn fallback_stub(Path(segment): Path<String>) -> ResponseJson<Value> {
    let table = match segment.as_str() {
        "organization_members" => "organization_member_metadata",
        "user_workspaces" | "project_workspaces" => "workspaces",
        other => other,
    };
    snapshot(table, Vec::<Value>::new())
}
