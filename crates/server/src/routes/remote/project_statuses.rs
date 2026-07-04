use api_types::ListProjectStatusesResponse;
use axum::{
    Router,
    extract::{Query, State},
    response::Json as ResponseJson,
    routing::get,
};
use db::models::board::ProjectStatuses;
use deployment::Deployment;
use serde::Deserialize;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

#[derive(Debug, Deserialize)]
pub(super) struct ListProjectStatusesQuery {
    pub project_id: Uuid,
}

pub(super) fn router() -> Router<DeploymentImpl> {
    Router::new().route("/project-statuses", get(list_project_statuses))
}

async fn list_project_statuses(
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<ListProjectStatusesQuery>,
) -> Result<ResponseJson<ApiResponse<ListProjectStatusesResponse>>, ApiError> {
    let project_statuses =
        ProjectStatuses::list_by_project(&deployment.db().pool, query.project_id).await?;
    Ok(ResponseJson(ApiResponse::success(
        ListProjectStatusesResponse { project_statuses },
    )))
}
