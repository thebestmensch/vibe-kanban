use api_types::{ListProjectsResponse, Project};
use axum::{
    Router,
    extract::{Path, Query, State},
    response::Json as ResponseJson,
    routing::get,
};
use db::models::board::BoardProjects;
use deployment::Deployment;
use serde::Deserialize;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

#[derive(Debug, Deserialize)]
pub(super) struct ListRemoteProjectsQuery {
    pub organization_id: Uuid,
}

pub(super) fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/projects", get(list_remote_projects))
        .route("/projects/{project_id}", get(get_remote_project))
}

async fn list_remote_projects(
    State(deployment): State<DeploymentImpl>,
    Query(query): Query<ListRemoteProjectsQuery>,
) -> Result<ResponseJson<ApiResponse<ListProjectsResponse>>, ApiError> {
    let projects = BoardProjects::list_by_org(&deployment.db().pool, query.organization_id).await?;
    Ok(ResponseJson(ApiResponse::success(ListProjectsResponse {
        projects,
    })))
}

async fn get_remote_project(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<Project>>, ApiError> {
    let project = BoardProjects::get(&deployment.db().pool, project_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    Ok(ResponseJson(ApiResponse::success(project)))
}
