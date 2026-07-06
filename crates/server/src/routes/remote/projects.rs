use api_types::{ListProjectsResponse, Project};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::Json as ResponseJson,
    routing::get,
};
use db::models::board::BoardProjects;
use deployment::Deployment;
use executors::{
    executors::BaseCodingAgent,
    profile::{ExecutorConfigs, canonical_variant_key},
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

#[derive(Debug, Deserialize)]
pub(super) struct ListRemoteProjectsQuery {
    pub organization_id: Uuid,
}

/// A project's per-project default Claude executor variant (JM-735), so the
/// settings editor can pre-fill the selector on open. Kept as its own view (not
/// a field on `Project`) so the remote/Electric project shape stays untouched.
#[derive(Debug, Serialize, TS)]
pub struct ProjectClaudeVariantView {
    pub variant: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
pub struct SetClaudeVariantBody {
    /// `None` clears the per-project default (fall through to the global one).
    pub variant: Option<String>,
}

pub(super) fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/projects", get(list_remote_projects))
        .route("/projects/{project_id}", get(get_remote_project))
        .route(
            "/projects/{project_id}/claude-variant",
            get(get_project_claude_variant).put(set_project_claude_variant),
        )
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

async fn get_project_claude_variant(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<ProjectClaudeVariantView>>, ApiError> {
    let variant = BoardProjects::claude_account_variant(&deployment.db().pool, project_id).await?;
    Ok(ResponseJson(ApiResponse::success(
        ProjectClaudeVariantView { variant },
    )))
}

/// Set (or clear) a project's default Claude variant (JM-735). Rejects a variant
/// that isn't a live CLAUDE_CODE profile so a bad bind can't persist and later
/// fail the agent at spawn (`get_coding_agent` errors on an unknown variant);
/// mirrors the Linear analog's save-time account-key validation.
async fn set_project_claude_variant(
    State(deployment): State<DeploymentImpl>,
    Path(project_id): Path<Uuid>,
    Json(body): Json<SetClaudeVariantBody>,
) -> Result<ResponseJson<ApiResponse<ProjectClaudeVariantView>>, ApiError> {
    // Canonicalize before persisting: profile lookups at spawn resolve variants
    // by EXACT key (`get_variant`), so a raw "work" must be stored as the profile
    // key "WORK" — otherwise validation passes but the later spawn fails on an
    // unknown variant. `None` clears the binding.
    let variant = match body.variant.as_deref() {
        Some(raw) => {
            let key = canonical_variant_key(raw);
            let known = ExecutorConfigs::get_cached()
                .executors
                .get(&BaseCodingAgent::ClaudeCode)
                .is_some_and(|profile| profile.configurations.contains_key(&key));
            if !known {
                return Err(ApiError::BadRequest(format!(
                    "unknown Claude variant '{raw}' — not a CLAUDE_CODE profile variant"
                )));
            }
            Some(key)
        }
        None => None,
    };
    BoardProjects::set_claude_account_variant(&deployment.db().pool, project_id, variant.clone())
        .await?;
    Ok(ResponseJson(ApiResponse::success(
        ProjectClaudeVariantView { variant },
    )))
}
