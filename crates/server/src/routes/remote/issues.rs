use api_types::{
    CreateIssueRequest, Issue, ListIssuesQuery, ListIssuesResponse, MutationResponse,
    SearchIssuesRequest, UpdateIssueRequest,
};
use axum::{
    Router,
    extract::{Json, Path, Query, State},
    response::Json as ResponseJson,
    routing::{get, post},
};
use db::{LOCAL_USER_ID, models::board::Issues};
use deployment::Deployment;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

pub(super) fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/issues", get(list_issues).post(create_issue))
        .route("/issues/search", post(search_issues))
        .route(
            "/issues/{issue_id}",
            get(get_issue).patch(update_issue).delete(delete_issue),
        )
}

async fn list_issues(
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<ListIssuesQuery>,
) -> Result<ResponseJson<ApiResponse<ListIssuesResponse>>, ApiError> {
    let response = Issues::list_by_project(&deployment.db().pool, query.project_id).await?;
    Ok(ResponseJson(ApiResponse::success(response)))
}

async fn search_issues(
    State(deployment): State<DeploymentImpl>,
    Json(request): Json<SearchIssuesRequest>,
) -> Result<ResponseJson<ApiResponse<ListIssuesResponse>>, ApiError> {
    let response = Issues::search(&deployment.db().pool, &request).await?;
    Ok(ResponseJson(ApiResponse::success(response)))
}

async fn get_issue(
    State(deployment): State<DeploymentImpl>,
    Path(issue_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<Issue>>, ApiError> {
    let issue = Issues::get(&deployment.db().pool, issue_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(ResponseJson(ApiResponse::success(issue)))
}

async fn create_issue(
    State(deployment): State<DeploymentImpl>,
    Json(request): Json<CreateIssueRequest>,
) -> Result<ResponseJson<ApiResponse<MutationResponse<Issue>>>, ApiError> {
    let issue = Issues::create(&deployment.db().pool, &request, Some(LOCAL_USER_ID)).await?;
    Ok(ResponseJson(ApiResponse::success(local_mutation(issue))))
}

async fn update_issue(
    State(deployment): State<DeploymentImpl>,
    Path(issue_id): Path<Uuid>,
    Json(request): Json<UpdateIssueRequest>,
) -> Result<ResponseJson<ApiResponse<MutationResponse<Issue>>>, ApiError> {
    let issue = Issues::update(&deployment.db().pool, issue_id, &request).await?;
    Ok(ResponseJson(ApiResponse::success(local_mutation(issue))))
}

async fn delete_issue(
    State(deployment): State<DeploymentImpl>,
    Path(issue_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<()>>, ApiError> {
    Issues::delete(&deployment.db().pool, issue_id).await?;
    Ok(ResponseJson(ApiResponse::success(())))
}

/// Wrap a persisted entity in the cloud mutation envelope. `txid` is meaningless
/// locally (single writer, synchronous commit — persistence == this 200), so it's
/// a fixed 0; the frontend no longer awaits it after the Electric removal.
fn local_mutation(data: Issue) -> MutationResponse<Issue> {
    MutationResponse { data, txid: 0 }
}
