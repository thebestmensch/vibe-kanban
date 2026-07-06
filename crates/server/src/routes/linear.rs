//! Linear integration routes (JM-718 slice 4): account connect/list/remove,
//! the column→state map editor, per-project account binding, and manual
//! card↔issue linking by identifier.
//!
//! Credential discipline (ADR 0002): account tokens live in `config.json` but
//! **never leave over the wire**. Every response here returns [`LinearAccountView`]
//! (a `has_token` boolean, not the secret), and account writes go through these
//! dedicated routes — the generic `PUT /config` refuses to touch `linear`.

use std::collections::HashMap;

use axum::{
    Router,
    extract::{Path, State},
    response::Json as ResponseJson,
    routing::{get, post, put},
};
use db::models::board::{BoardProjects, Issues};
use deployment::Deployment;
use linear::{LinearClient, LinearError};
use serde::{Deserialize, Serialize};
use services::services::config::{LinearAccount, LinearConfig, save_config_to_file};
use ts_rs::TS;
use utils::{assets::config_path, response::ApiResponse};
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

pub fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/linear/accounts", get(list_accounts).post(connect_account))
        .route(
            "/linear/accounts/{key}",
            axum::routing::delete(remove_account),
        )
        .route("/linear/accounts/{key}/state-map", put(set_state_map))
        .route(
            "/linear/accounts/{key}/workflow-states",
            get(list_workflow_states),
        )
        .route(
            "/linear/projects/{id}/account",
            get(get_project_binding).put(bind_project),
        )
        .route("/linear/projects/{id}/links", get(list_project_links))
        .route(
            "/linear/issues/{id}/link",
            post(link_issue).delete(unlink_issue),
        )
}

// --- Wire types -------------------------------------------------------------

/// A Linear account as returned to the client: the token is represented only by
/// `has_token`, never echoed.
#[derive(Debug, Serialize, TS)]
pub struct LinearAccountView {
    pub key: String,
    pub workspace_name: Option<String>,
    pub team_id: Option<String>,
    pub has_token: bool,
    pub state_map: HashMap<String, String>,
}

impl LinearAccountView {
    fn of(key: &str, account: &LinearAccount) -> Self {
        Self {
            key: key.to_string(),
            workspace_name: account.workspace_name.clone(),
            team_id: account.team_id.clone(),
            has_token: account.token.is_some(),
            state_map: account.state_map.clone(),
        }
    }
}

#[derive(Debug, Deserialize, TS)]
pub struct ConnectLinearAccountBody {
    pub key: String,
    pub token: String,
    pub workspace_name: Option<String>,
    pub team_id: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
pub struct SetStateMapBody {
    /// `project_statuses.id` (UUID string) → Linear workflow-state id.
    pub state_map: HashMap<String, String>,
}

#[derive(Debug, Serialize, TS)]
pub struct LinearWorkflowStateView {
    pub id: String,
    pub name: String,
    pub state_type: String,
    pub position: f64,
}

#[derive(Debug, Deserialize, TS)]
pub struct BindProjectBody {
    /// `None` unbinds the project.
    pub account_key: Option<String>,
}

/// A project's current Linear account binding, so the settings editor can
/// pre-fill the account dropdown and column→state grid on open.
#[derive(Debug, Serialize, TS)]
pub struct ProjectLinearBindingView {
    pub account_key: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
pub struct LinkIssueBody {
    /// Team-key identifier, e.g. `OOM-123`.
    pub identifier: String,
}

#[derive(Debug, Serialize, TS)]
pub struct IssueLinkView {
    pub linear_issue_id: String,
    pub linear_issue_identifier: String,
    pub linear_url: String,
    pub linear_state_id: Option<String>,
}

/// A linked card's badge projection, keyed by `issue_id` so the board can merge
/// it onto the card. Local-only — kept off the shared `api_types::Issue` (which
/// the cloud remote repo also uses) per ADR 0002 / the JM-718 slice-5 review.
#[derive(Debug, Serialize, TS)]
pub struct LinkedIssueView {
    pub issue_id: String,
    pub linear_issue_identifier: String,
    pub linear_url: String,
    pub linear_sync_pending: bool,
}

// --- Config persistence -----------------------------------------------------

/// Mutate the persisted `linear` config atomically w.r.t. the config lock: clone
/// under the write guard, apply `mutate`, write to disk, and only commit the
/// in-memory copy if the disk write succeeded (so a failed save never leaves
/// memory ahead of disk).
async fn persist_linear<F>(deployment: &DeploymentImpl, mutate: F) -> Result<(), ApiError>
where
    F: FnOnce(&mut LinearConfig),
{
    let mut guard = deployment.config().write().await;
    let mut next = guard.clone();
    mutate(&mut next.linear);
    save_config_to_file(&next, &config_path())
        .await
        .map_err(|e| ApiError::BadRequest(format!("failed to save config: {e}")))?;
    *guard = next;
    Ok(())
}

// --- Account routes ---------------------------------------------------------

async fn list_accounts(
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<Vec<LinearAccountView>>>, ApiError> {
    let cfg = deployment.config().read().await;
    let mut views: Vec<LinearAccountView> = cfg
        .linear
        .accounts
        .iter()
        .map(|(k, a)| LinearAccountView::of(k, a))
        .collect();
    views.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(ResponseJson(ApiResponse::success(views)))
}

async fn connect_account(
    State(deployment): State<DeploymentImpl>,
    axum::extract::Json(body): axum::extract::Json<ConnectLinearAccountBody>,
) -> Result<ResponseJson<ApiResponse<LinearAccountView>>, ApiError> {
    if body.key.trim().is_empty() {
        return Err(ApiError::BadRequest("account key must not be empty".into()));
    }
    if body.token.trim().is_empty() {
        return Err(ApiError::BadRequest("token must not be empty".into()));
    }

    let key = body.key.trim().to_string();
    // Preserve an existing account's state_map on re-connect (updating a token
    // shouldn't wipe the column mapping).
    let existing_state_map = deployment
        .config()
        .read()
        .await
        .linear
        .accounts
        .get(&key)
        .map(|a| a.state_map.clone())
        .unwrap_or_default();

    let account = LinearAccount {
        token: Some(body.token),
        workspace_name: body.workspace_name,
        team_id: body.team_id,
        state_map: existing_state_map,
    };
    let view = LinearAccountView::of(&key, &account);

    persist_linear(&deployment, |linear| {
        linear.accounts.insert(key, account);
    })
    .await?;

    Ok(ResponseJson(ApiResponse::success(view)))
}

async fn remove_account(
    State(deployment): State<DeploymentImpl>,
    Path(key): Path<String>,
) -> Result<ResponseJson<ApiResponse<()>>, ApiError> {
    persist_linear(&deployment, |linear| {
        linear.accounts.remove(&key);
    })
    .await?;
    Ok(ResponseJson(ApiResponse::success(())))
}

async fn set_state_map(
    State(deployment): State<DeploymentImpl>,
    Path(key): Path<String>,
    axum::extract::Json(body): axum::extract::Json<SetStateMapBody>,
) -> Result<ResponseJson<ApiResponse<LinearAccountView>>, ApiError> {
    if !deployment
        .config()
        .read()
        .await
        .linear
        .accounts
        .contains_key(&key)
    {
        return Err(ApiError::BadRequest(format!(
            "unknown Linear account '{key}'"
        )));
    }
    persist_linear(&deployment, |linear| {
        if let Some(account) = linear.accounts.get_mut(&key) {
            account.state_map = body.state_map;
        }
    })
    .await?;

    let cfg = deployment.config().read().await;
    let account = cfg
        .linear
        .accounts
        .get(&key)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown Linear account '{key}'")))?;
    Ok(ResponseJson(ApiResponse::success(LinearAccountView::of(
        &key, account,
    ))))
}

/// Fetch a team's workflow states from Linear so the client can build the
/// column→state map. Doubles as a connection/credential check.
async fn list_workflow_states(
    State(deployment): State<DeploymentImpl>,
    Path(key): Path<String>,
) -> Result<ResponseJson<ApiResponse<Vec<LinearWorkflowStateView>>>, ApiError> {
    // Extract token + team under a short read guard, then release before I/O.
    let (token, team_id) = {
        let cfg = deployment.config().read().await;
        let account = cfg
            .linear
            .accounts
            .get(&key)
            .ok_or_else(|| ApiError::BadRequest(format!("unknown Linear account '{key}'")))?;
        let token = account
            .token
            .clone()
            .ok_or_else(|| ApiError::BadRequest(format!("account '{key}' has no token")))?;
        let team_id = account.team_id.clone().ok_or_else(|| {
            ApiError::BadRequest(format!("account '{key}' has no team configured"))
        })?;
        (token, team_id)
    };

    let states = LinearClient::new(token)
        .list_workflow_states(&team_id)
        .await
        .map_err(linear_err)?;
    let views = states
        .into_iter()
        .map(|s| LinearWorkflowStateView {
            id: s.id,
            name: s.name,
            state_type: s.state_type,
            position: s.position,
        })
        .collect();
    Ok(ResponseJson(ApiResponse::success(views)))
}

// --- Project binding --------------------------------------------------------

/// Read a project's current Linear account binding (`id` is the project id).
async fn get_project_binding(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<ProjectLinearBindingView>>, ApiError> {
    let account_key = BoardProjects::linear_account_key(&deployment.db().pool, id).await?;
    Ok(ResponseJson(ApiResponse::success(
        ProjectLinearBindingView { account_key },
    )))
}

async fn bind_project(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
    axum::extract::Json(body): axum::extract::Json<BindProjectBody>,
) -> Result<ResponseJson<ApiResponse<()>>, ApiError> {
    // Reject binding to an account key that doesn't exist (typo guard).
    if let Some(key) = &body.account_key
        && !deployment
            .config()
            .read()
            .await
            .linear
            .accounts
            .contains_key(key)
    {
        return Err(ApiError::BadRequest(format!(
            "unknown Linear account '{key}'"
        )));
    }
    BoardProjects::set_linear_account_key(&deployment.db().pool, id, body.account_key).await?;
    Ok(ResponseJson(ApiResponse::success(())))
}

/// List the Linear-link projections for a project's linked cards, for the board
/// badge. `id` is the project id.
async fn list_project_links(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<Vec<LinkedIssueView>>>, ApiError> {
    let rows = Issues::list_linear_links(&deployment.db().pool, id).await?;
    let views = rows
        .into_iter()
        .map(|r| LinkedIssueView {
            issue_id: r.issue_id.to_string(),
            linear_issue_identifier: r.linear_issue_identifier,
            linear_url: r.linear_url,
            linear_sync_pending: r.linear_sync_pending != 0,
        })
        .collect();
    Ok(ResponseJson(ApiResponse::success(views)))
}

// --- Manual link ------------------------------------------------------------

async fn link_issue(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
    axum::extract::Json(body): axum::extract::Json<LinkIssueBody>,
) -> Result<ResponseJson<ApiResponse<IssueLinkView>>, ApiError> {
    let identifier = body.identifier.trim().to_string();
    if identifier.is_empty() {
        return Err(ApiError::BadRequest("identifier must not be empty".into()));
    }

    let issue = Issues::get(&deployment.db().pool, id)
        .await?
        .ok_or_else(|| ApiError::BadRequest(format!("no such card {id}")))?;

    let account_key = BoardProjects::linear_account_key(&deployment.db().pool, issue.project_id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest(
                "this card's board is not bound to a Linear account; bind it first".into(),
            )
        })?;

    // Resolve token + expected team + the card's mapped target under a short read
    // guard, then release before network I/O. `mapped_target` is the Linear state
    // the card's current column maps to (if mapped) — it drives the board-wins
    // reconciliation below.
    let (token, expected_team, mapped_target) = {
        let cfg = deployment.config().read().await;
        let account = cfg.linear.accounts.get(&account_key).ok_or_else(|| {
            ApiError::BadRequest(format!("board is bound to unknown account '{account_key}'"))
        })?;
        let token = account
            .token
            .clone()
            .ok_or_else(|| ApiError::BadRequest(format!("account '{account_key}' has no token")))?;
        let mapped_target = account.state_map.get(&issue.status_id.to_string()).cloned();
        (token, account.team_id.clone(), mapped_target)
    };

    let resolved = LinearClient::new(token)
        .resolve_issue_by_identifier(&identifier)
        .await
        .map_err(linear_err)?;

    // Guard against linking an issue from a different team than the account is
    // configured for (its state map would never apply).
    if let Some(team) = &expected_team
        && &resolved.team_id != team
    {
        return Err(ApiError::BadRequest(format!(
            "{identifier} belongs to a different Linear team than account '{account_key}'"
        )));
    }

    // Board-wins reconciliation on link: if the card already sits in a mapped
    // column whose target differs from the issue's current Linear state, queue an
    // outbound push so linking doesn't leave the two silently diverged.
    let sync_pending = mapped_target
        .as_deref()
        .is_some_and(|target| Some(target) != resolved.state_id.as_deref());

    match Issues::link_linear(
        &deployment.db().pool,
        id,
        &resolved.id,
        &resolved.identifier,
        &resolved.url,
        resolved.state_id.as_deref(),
        sync_pending,
    )
    .await
    {
        Ok(()) => {
            if sync_pending {
                deployment.trigger_linear_sync();
            }
            Ok(ResponseJson(ApiResponse::success(IssueLinkView {
                linear_issue_id: resolved.id,
                linear_issue_identifier: resolved.identifier,
                linear_url: resolved.url,
                linear_state_id: resolved.state_id,
            })))
        }
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(ApiError::Conflict(
            format!("{identifier} is already linked to another card"),
        )),
        Err(e) => Err(e.into()),
    }
}

async fn unlink_issue(
    State(deployment): State<DeploymentImpl>,
    Path(id): Path<Uuid>,
) -> Result<ResponseJson<ApiResponse<()>>, ApiError> {
    Issues::unlink_linear(&deployment.db().pool, id).await?;
    Ok(ResponseJson(ApiResponse::success(())))
}

/// Map a `LinearError` to an `ApiError`: auth/not-found are the client's problem
/// (4xx), everything else is an upstream/transport failure (bad gateway).
fn linear_err(e: LinearError) -> ApiError {
    match e {
        LinearError::AuthFailed(m) => ApiError::Forbidden(format!("Linear auth failed: {m}")),
        LinearError::NotFound(m) => ApiError::BadRequest(format!("not found in Linear: {m}")),
        other => ApiError::BadGateway(format!("Linear API error: {other}")),
    }
}
