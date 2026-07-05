//! Local board write path (JM-714 v1).
//!
//! Mirrors the mutation URLs the frontend's generic collection handlers and
//! explicit bulk helpers target (`buildMutationHandlers` in
//! `collections.ts`, `bulkUpdate*` in `remoteApi.ts`): `POST /v1/<table>`
//! (insert, body = the full optimistic row), `PATCH /v1/<table>/{id}` (single
//! partial update, body = changes), `POST /v1/<table>/bulk` (body =
//! `{ updates: [{ id, ...changes }] }`), `DELETE /v1/<table>/{id}`.
//!
//! Every handler returns `{ "txid": 0 }`: in local fallback-locked mode the
//! frontend ignores the txid and re-fetches the `/v1/fallback/*` snapshot after
//! each mutation, so the persisted row is the source of truth. Insert bodies are
//! full Issue/ProjectStatus rows; the Create request types ignore the unknown
//! server-assigned fields (`issue_number`, `simple_id`, timestamps) and honor
//! the client-generated `id` for stable optimistic reconciliation.

use api_types::{
    CreateIssueRequest, CreateProjectStatusRequest, UpdateIssueRequest, UpdateProjectStatusRequest,
};
use axum::{
    Router,
    extract::{Json, Path, State},
    response::Json as ResponseJson,
    routing::{patch, post},
};
use db::{
    LOCAL_USER_ID,
    models::board::{Issues, ProjectStatuses},
};
use deployment::Deployment;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

pub(super) fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/issues", post(create_issue))
        .route("/issues/bulk", post(bulk_update_issues))
        .route("/issues/{id}", patch(update_issue).delete(delete_issue))
        .route("/project_statuses", post(create_status))
        .route("/project_statuses/bulk", post(bulk_update_statuses))
        .route(
            "/project_statuses/{id}",
            patch(update_status).delete(delete_status),
        )
}

/// The mutation ack. `txid` is meaningless locally (single writer, synchronous
/// commit) and ignored by the fallback-locked frontend, which re-reads the
/// snapshot instead.
fn ack() -> ResponseJson<Value> {
    ResponseJson(json!({ "txid": 0 }))
}

#[derive(Debug, Deserialize)]
struct BulkBody {
    updates: Vec<Value>,
}

/// Split a `{ id, ...changes }` bulk item into its `id` and the remaining
/// change fields re-parsed as `T`. Extracting rather than `#[serde(flatten)]`
/// avoids the known flatten × `deserialize_with` interaction — the Update types
/// use custom `some_if_present` deserializers that misbehave under flatten.
fn split_bulk_item<T: for<'de> Deserialize<'de>>(item: Value) -> Result<(Uuid, T), ApiError> {
    let id = item
        .get("id")
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| ApiError::BadRequest("bulk update item missing valid `id`".to_string()))?;
    let changes = serde_json::from_value(item)
        .map_err(|e| ApiError::BadRequest(format!("invalid bulk update item: {e}")))?;
    Ok((id, changes))
}

// --- Issues -----------------------------------------------------------------

async fn create_issue(
    State(deployment): State<DeploymentImpl>,
    Json(request): Json<CreateIssueRequest>,
) -> Result<ResponseJson<Value>, ApiError> {
    Issues::create(&deployment.db().pool, &request, Some(LOCAL_USER_ID)).await?;
    Ok(ack())
}

async fn update_issue(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateIssueRequest>,
) -> Result<ResponseJson<Value>, ApiError> {
    Issues::update(&deployment.db().pool, id, &request).await?;
    Ok(ack())
}

async fn delete_issue(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
) -> Result<ResponseJson<Value>, ApiError> {
    Issues::delete(&deployment.db().pool, id).await?;
    Ok(ack())
}

async fn bulk_update_issues(
    State(deployment): State<DeploymentImpl>,
    Json(body): Json<BulkBody>,
) -> Result<ResponseJson<Value>, ApiError> {
    let updates = body
        .updates
        .into_iter()
        .map(split_bulk_item::<UpdateIssueRequest>)
        .collect::<Result<Vec<_>, _>>()?;
    Issues::bulk_update(&deployment.db().pool, &updates).await?;
    Ok(ack())
}

// --- Project statuses -------------------------------------------------------

async fn create_status(
    State(deployment): State<DeploymentImpl>,
    Json(request): Json<CreateProjectStatusRequest>,
) -> Result<ResponseJson<Value>, ApiError> {
    ProjectStatuses::create(&deployment.db().pool, &request).await?;
    Ok(ack())
}

async fn update_status(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateProjectStatusRequest>,
) -> Result<ResponseJson<Value>, ApiError> {
    ProjectStatuses::update(&deployment.db().pool, id, &request).await?;
    Ok(ack())
}

async fn delete_status(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
) -> Result<ResponseJson<Value>, ApiError> {
    // `issues.status_id` REFERENCES `project_statuses(id)` with no ON DELETE
    // action, and sqlx enforces foreign keys, so deleting a column that still
    // has cards fails closed (no orphans). Translate that raw FK error into a
    // clear 409 instead of an opaque 500.
    match ProjectStatuses::delete(&deployment.db().pool, id).await {
        Ok(_) => Ok(ack()),
        Err(sqlx::Error::Database(e)) if e.is_foreign_key_violation() => Err(ApiError::Conflict(
            "cannot delete a column that still has cards; move or delete them first".to_string(),
        )),
        Err(e) => Err(e.into()),
    }
}

async fn bulk_update_statuses(
    State(deployment): State<DeploymentImpl>,
    Json(body): Json<BulkBody>,
) -> Result<ResponseJson<Value>, ApiError> {
    let updates = body
        .updates
        .into_iter()
        .map(split_bulk_item::<UpdateProjectStatusRequest>)
        .collect::<Result<Vec<_>, _>>()?;
    ProjectStatuses::bulk_update(&deployment.db().pool, &updates).await?;
    Ok(ack())
}
