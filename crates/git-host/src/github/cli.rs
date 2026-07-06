//! Minimal helpers around the GitHub CLI (`gh`).
//!
//! This module provides low-level access to the GitHub CLI for operations
//! the REST client does not cover well.

use std::{
    ffi::{OsStr, OsString},
    io::Write,
    path::Path,
    process::Command,
};

use chrono::{DateTime, Utc};
use db::models::merge::{CheckStatus, MergeStatus};
use serde::Deserialize;
use tempfile::NamedTempFile;
use thiserror::Error;
use url::Url;
use utils::{command_ext::NoWindowExt, shell::resolve_executable_path_blocking};

use crate::types::{
    CreatePrRequest, PrComment, PrCommentAuthor, PrReviewComment, PullRequestDetail,
    ReviewCommentUser,
};

#[derive(Debug, Clone)]
pub struct GitHubRepoInfo {
    pub owner: String,
    pub repo_name: String,
    /// GitHub hostname (e.g., "github.com" or enterprise hostname)
    pub hostname: Option<String>,
}

impl GitHubRepoInfo {
    pub fn repo_spec(&self) -> String {
        match &self.hostname {
            Some(host) => format!("{}/{}/{}", host, self.owner, self.repo_name),
            None => format!("{}/{}", self.owner, self.repo_name),
        }
    }
}

#[derive(Deserialize)]
struct GhRepoViewResponse {
    owner: GhRepoOwner,
    name: String,
    url: String,
}

#[derive(Deserialize)]
struct GhRepoOwner {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhCommentResponse {
    id: String,
    author: Option<GhUserLogin>,
    #[serde(default)]
    author_association: String,
    #[serde(default)]
    body: String,
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
struct GhCommentsWrapper {
    comments: Vec<GhCommentResponse>,
}

#[derive(Deserialize)]
struct GhUserLogin {
    login: Option<String>,
}

#[derive(Deserialize)]
struct GhReviewCommentResponse {
    id: i64,
    user: Option<GhUserLogin>,
    #[serde(default)]
    body: String,
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    path: String,
    line: Option<i64>,
    side: Option<String>,
    #[serde(default)]
    diff_hunk: String,
    #[serde(default)]
    author_association: String,
}

#[derive(Deserialize)]
struct GhMergeCommit {
    oid: Option<String>,
}

/// One element of GitHub's `statusCheckRollup` array. The array is heterogeneous:
/// `CheckRun` elements (GitHub Actions / Checks API) carry `status` + `conclusion`,
/// while `StatusContext` elements (legacy commit statuses, e.g. CodeRabbit) carry
/// only `state`. Discriminating on `__typename` is required — a single struct with
/// optional `state`/`conclusion` would silently read every mismatched element as
/// absent, producing false-green rollups.
#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum GhCheckElement {
    CheckRun {
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        conclusion: Option<String>,
    },
    StatusContext {
        #[serde(default)]
        state: Option<String>,
    },
    /// Any future/unrecognized rollup element type. Treated as neutral so a new
    /// GitHub typename can never be misread as a hard failure or pass.
    #[serde(other)]
    Unknown,
}

/// Per-element classification, reduced across the rollup by `rollup_check_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementClass {
    Failing,
    Pending,
    Passing,
    /// Intentionally-not-run (skipped/neutral/stale-context) — ignored in the reduce.
    Neutral,
}

fn classify_element(el: &GhCheckElement) -> ElementClass {
    match el {
        GhCheckElement::CheckRun { status, conclusion } => {
            let status = status.as_deref().unwrap_or("").to_ascii_uppercase();
            // Anything not COMPLETED is still running/queued → pending.
            if status != "COMPLETED" {
                return ElementClass::Pending;
            }
            match conclusion
                .as_deref()
                .unwrap_or("")
                .to_ascii_uppercase()
                .as_str()
            {
                "SUCCESS" => ElementClass::Passing,
                "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE"
                | "STALE" => ElementClass::Failing,
                "SKIPPED" | "NEUTRAL" => ElementClass::Neutral,
                // COMPLETED with a missing/unknown conclusion — don't guess green.
                _ => ElementClass::Pending,
            }
        }
        GhCheckElement::StatusContext { state } => {
            match state.as_deref().unwrap_or("").to_ascii_uppercase().as_str() {
                "SUCCESS" => ElementClass::Passing,
                "FAILURE" | "ERROR" => ElementClass::Failing,
                "PENDING" | "EXPECTED" => ElementClass::Pending,
                _ => ElementClass::Neutral,
            }
        }
        GhCheckElement::Unknown => ElementClass::Neutral,
    }
}

/// Reduce a `statusCheckRollup` array to a single [`CheckStatus`].
///
/// Precedence: any failing → `Failing`; else any pending → `Pending`; else any
/// passing → `Passing`; else `NoChecks` (empty rollup, or only skipped/neutral
/// elements). Failing dominates so a red required check is never masked by a
/// later success.
fn rollup_check_status(elements: &[GhCheckElement]) -> CheckStatus {
    let mut any_pending = false;
    let mut any_passing = false;
    for el in elements {
        match classify_element(el) {
            ElementClass::Failing => return CheckStatus::Failing,
            ElementClass::Pending => any_pending = true,
            ElementClass::Passing => any_passing = true,
            ElementClass::Neutral => {}
        }
    }
    if any_pending {
        CheckStatus::Pending
    } else if any_passing {
        CheckStatus::Passing
    } else {
        CheckStatus::NoChecks
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPrResponse {
    number: i64,
    url: String,
    #[serde(default)]
    state: String,
    merged_at: Option<DateTime<Utc>>,
    merge_commit: Option<GhMergeCommit>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    base_ref_name: Option<String>,
    #[serde(default)]
    head_ref_name: Option<String>,
    #[serde(default)]
    updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    status_check_rollup: Vec<GhCheckElement>,
    #[serde(default)]
    merge_state_status: Option<String>,
}

#[derive(Debug, Error)]
pub enum GhCliError {
    #[error("GitHub CLI (`gh`) executable not found or not runnable")]
    NotAvailable,
    #[error("GitHub CLI command failed: {0}")]
    CommandFailed(String),
    #[error("GitHub CLI authentication failed: {0}")]
    AuthFailed(String),
    #[error("GitHub CLI returned unexpected output: {0}")]
    UnexpectedOutput(String),
}

#[derive(Debug, Clone, Default)]
pub struct GhCli;

impl GhCli {
    pub fn new() -> Self {
        Self {}
    }

    /// Ensure the GitHub CLI binary is discoverable.
    fn ensure_available(&self) -> Result<(), GhCliError> {
        resolve_executable_path_blocking("gh").ok_or(GhCliError::NotAvailable)?;
        Ok(())
    }

    fn run<I, S>(&self, args: I, dir: Option<&Path>) -> Result<String, GhCliError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.ensure_available()?;
        let gh = resolve_executable_path_blocking("gh").ok_or(GhCliError::NotAvailable)?;
        let mut cmd = Command::new(&gh);
        if let Some(d) = dir {
            cmd.current_dir(d);
        }
        for arg in args {
            cmd.arg(arg);
        }
        let output = cmd
            .no_window()
            .output()
            .map_err(|err| GhCliError::CommandFailed(err.to_string()))?;

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).to_string());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        // Check exit code first - gh CLI uses exit code 4 for auth failures
        if output.status.code() == Some(4) {
            return Err(GhCliError::AuthFailed(stderr));
        }

        // Fall back to string matching for older gh versions or other auth scenarios
        let lower = stderr.to_ascii_lowercase();
        if lower.contains("authentication failed")
            || lower.contains("must authenticate")
            || lower.contains("bad credentials")
            || lower.contains("unauthorized")
            || lower.contains("gh auth login")
        {
            return Err(GhCliError::AuthFailed(stderr));
        }

        Err(GhCliError::CommandFailed(stderr))
    }

    pub fn get_repo_info(
        &self,
        remote_url: &str,
        repo_path: &Path,
    ) -> Result<GitHubRepoInfo, GhCliError> {
        let raw = self.run(
            ["repo", "view", remote_url, "--json", "owner,name,url"],
            Some(repo_path),
        )?;
        Self::parse_repo_info_response(&raw)
    }

    fn parse_repo_info_response(raw: &str) -> Result<GitHubRepoInfo, GhCliError> {
        let resp: GhRepoViewResponse = serde_json::from_str(raw).map_err(|e| {
            GhCliError::UnexpectedOutput(format!("Failed to parse gh repo view response: {e}"))
        })?;

        let hostname = Url::parse(&resp.url)
            .ok()
            .and_then(|u| u.host_str().map(String::from));

        Ok(GitHubRepoInfo {
            owner: resp.owner.login,
            repo_name: resp.name,
            hostname,
        })
    }

    /// Run `gh pr create` and parse the response.
    ///
    /// The `repo_path` parameter specifies the working directory for the command.
    /// This is required for compatibility with older `gh` CLI versions (e.g., v2.4.0)
    /// that require running from within a git repository.
    pub fn create_pr(
        &self,
        request: &CreatePrRequest,
        repo_info: &GitHubRepoInfo,
        repo_path: &Path,
    ) -> Result<PullRequestDetail, GhCliError> {
        // Write body to temp file to avoid shell escaping and length issues
        let body = request.body.as_deref().unwrap_or("");
        let mut body_file = NamedTempFile::new()
            .map_err(|e| GhCliError::CommandFailed(format!("Failed to create temp file: {e}")))?;
        body_file
            .write_all(body.as_bytes())
            .map_err(|e| GhCliError::CommandFailed(format!("Failed to write body: {e}")))?;

        let repo_spec = repo_info.repo_spec();

        let mut args: Vec<OsString> = Vec::with_capacity(14);
        args.push(OsString::from("pr"));
        args.push(OsString::from("create"));
        args.push(OsString::from("--repo"));
        args.push(OsString::from(&repo_spec));
        args.push(OsString::from("--head"));
        args.push(OsString::from(&request.head_branch));
        args.push(OsString::from("--base"));
        args.push(OsString::from(&request.base_branch));
        args.push(OsString::from("--title"));
        args.push(OsString::from(&request.title));
        args.push(OsString::from("--body-file"));
        args.push(body_file.path().as_os_str().to_os_string());

        if request.draft.unwrap_or(false) {
            args.push(OsString::from("--draft"));
        }

        let raw = self.run(args, Some(repo_path))?;
        Self::parse_pr_create_text(&raw, request)
    }

    /// Retrieve details for a pull request by URL.
    pub fn view_pr(&self, pr_url: &str) -> Result<PullRequestDetail, GhCliError> {
        let raw = self.run(
            [
                "pr",
                "view",
                pr_url,
                "--json",
                "number,url,state,mergedAt,mergeCommit,title,baseRefName,headRefName,statusCheckRollup,mergeStateStatus",
            ],
            None,
        )?;
        Self::parse_pr_view(&raw)
    }

    /// List pull requests for a branch (includes closed/merged).
    pub fn list_prs_for_branch(
        &self,
        repo_info: &GitHubRepoInfo,
        branch: &str,
    ) -> Result<Vec<PullRequestDetail>, GhCliError> {
        let repo_spec = repo_info.repo_spec();
        let raw = self.run(
            [
                "pr",
                "list",
                "--repo",
                &repo_spec,
                "--state",
                "all",
                "--head",
                branch,
                "--json",
                "number,url,title,headRefName,baseRefName,state,mergedAt,mergeCommit,statusCheckRollup,mergeStateStatus",
            ],
            None,
        )?;
        Self::parse_pr_list(&raw)
    }

    pub fn list_prs(&self, owner: &str, repo: &str) -> Result<Vec<PullRequestDetail>, GhCliError> {
        let repo_spec = format!("{owner}/{repo}");
        let json_fields = "number,url,title,headRefName,baseRefName,state,mergedAt,mergeCommit,updatedAt,statusCheckRollup,mergeStateStatus";

        let open_raw = self.run(
            [
                "pr",
                "list",
                "--repo",
                &repo_spec,
                "--state",
                "open",
                "--json",
                json_fields,
            ],
            None,
        )?;

        let closed_raw = self.run(
            [
                "pr",
                "list",
                "--repo",
                &repo_spec,
                "--state",
                "closed",
                "--limit",
                "20",
                "--json",
                json_fields,
            ],
            None,
        )?;

        let mut open_prs: Vec<GhPrResponse> =
            serde_json::from_str(open_raw.trim()).map_err(|err| {
                GhCliError::UnexpectedOutput(format!(
                    "Failed to parse gh pr list (open) response: {err}; raw: {open_raw}"
                ))
            })?;
        let closed_prs: Vec<GhPrResponse> =
            serde_json::from_str(closed_raw.trim()).map_err(|err| {
                GhCliError::UnexpectedOutput(format!(
                    "Failed to parse gh pr list (closed) response: {err}; raw: {closed_raw}"
                ))
            })?;

        open_prs.extend(closed_prs);
        open_prs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(open_prs
            .into_iter()
            .map(Self::pr_response_to_detail)
            .collect())
    }

    /// Fetch comments for a pull request.
    pub fn get_pr_comments(
        &self,
        repo_info: &GitHubRepoInfo,
        pr_number: i64,
    ) -> Result<Vec<PrComment>, GhCliError> {
        let repo_spec = repo_info.repo_spec();
        let raw = self.run(
            [
                "pr",
                "view",
                &pr_number.to_string(),
                "--repo",
                &repo_spec,
                "--json",
                "comments",
            ],
            None,
        )?;
        Self::parse_pr_comments(&raw)
    }

    /// Fetch inline review comments for a pull request via API.
    pub fn get_pr_review_comments(
        &self,
        repo_info: &GitHubRepoInfo,
        pr_number: i64,
    ) -> Result<Vec<PrReviewComment>, GhCliError> {
        let mut args = vec![
            "api".to_string(),
            format!(
                "repos/{}/{}/pulls/{}/comments",
                repo_info.owner, repo_info.repo_name, pr_number
            ),
        ];
        if let Some(ref host) = repo_info.hostname {
            args.push("--hostname".to_string());
            args.push(host.clone());
        }
        let raw = self.run(args, None)?;
        Self::parse_pr_review_comments(&raw)
    }

    pub fn pr_checkout(
        &self,
        repo_path: &Path,
        owner: &str,
        repo: &str,
        pr_number: i64,
    ) -> Result<(), GhCliError> {
        self.run(
            [
                "pr",
                "checkout",
                &pr_number.to_string(),
                "--repo",
                &format!("{owner}/{repo}"),
                "--force",
            ],
            Some(repo_path),
        )?;
        Ok(())
    }
}

impl GhCli {
    fn parse_pr_create_text(
        raw: &str,
        request: &CreatePrRequest,
    ) -> Result<PullRequestDetail, GhCliError> {
        let pr_url = raw
            .lines()
            .rev()
            .flat_map(|line| line.split_whitespace())
            .map(|token| token.trim_matches(|c: char| c == '<' || c == '>'))
            .find(|token| token.starts_with("http") && token.contains("/pull/"))
            .ok_or_else(|| {
                GhCliError::UnexpectedOutput(format!(
                    "gh pr create did not return a pull request URL; raw output: {raw}"
                ))
            })?
            .trim_end_matches(['.', ',', ';'])
            .to_string();

        let number = pr_url
            .rsplit('/')
            .next()
            .ok_or_else(|| {
                GhCliError::UnexpectedOutput(format!(
                    "Failed to extract PR number from URL '{pr_url}'"
                ))
            })?
            .trim_end_matches(|c: char| !c.is_ascii_digit())
            .parse::<i64>()
            .map_err(|err| {
                GhCliError::UnexpectedOutput(format!(
                    "Failed to parse PR number from URL '{pr_url}': {err}"
                ))
            })?;

        Ok(PullRequestDetail {
            number,
            url: pr_url,
            status: MergeStatus::Open,
            merged_at: None,
            merge_commit_sha: None,
            title: request.title.clone(),
            base_branch: request.base_branch.clone(),
            head_branch: request.head_branch.clone(),
            // A freshly-created PR has no rollup yet; the 60s poll fills this in.
            check_status: None,
            merge_state: None,
        })
    }

    fn parse_pr_view(raw: &str) -> Result<PullRequestDetail, GhCliError> {
        let pr: GhPrResponse = serde_json::from_str(raw.trim()).map_err(|err| {
            GhCliError::UnexpectedOutput(format!(
                "Failed to parse gh pr view response: {err}; raw: {raw}"
            ))
        })?;
        Ok(Self::pr_response_to_detail(pr))
    }

    fn parse_pr_list(raw: &str) -> Result<Vec<PullRequestDetail>, GhCliError> {
        let prs: Vec<GhPrResponse> = serde_json::from_str(raw.trim()).map_err(|err| {
            GhCliError::UnexpectedOutput(format!(
                "Failed to parse gh pr list response: {err}; raw: {raw}"
            ))
        })?;
        Ok(prs.into_iter().map(Self::pr_response_to_detail).collect())
    }

    fn pr_response_to_detail(pr: GhPrResponse) -> PullRequestDetail {
        let state = if pr.state.is_empty() {
            "OPEN"
        } else {
            &pr.state
        };
        let status = match state.to_ascii_uppercase().as_str() {
            "OPEN" => MergeStatus::Open,
            "MERGED" => MergeStatus::Merged,
            "CLOSED" => MergeStatus::Closed,
            _ => MergeStatus::Unknown,
        };
        // CI-check status is only meaningful while the PR is open (the review
        // loop). Once merged/closed the rollup is frozen/irrelevant, so leave it
        // absent rather than persist a stale terminal value.
        let check_status = matches!(status, MergeStatus::Open)
            .then(|| rollup_check_status(&pr.status_check_rollup));
        PullRequestDetail {
            number: pr.number,
            url: pr.url,
            status,
            merged_at: pr.merged_at,
            merge_commit_sha: pr.merge_commit.and_then(|c| c.oid),
            title: pr.title.unwrap_or_default(),
            base_branch: pr.base_ref_name.unwrap_or_default(),
            head_branch: pr.head_ref_name.unwrap_or_default(),
            check_status,
            merge_state: pr.merge_state_status,
        }
    }

    fn parse_pr_comments(raw: &str) -> Result<Vec<PrComment>, GhCliError> {
        let wrapper: GhCommentsWrapper = serde_json::from_str(raw.trim()).map_err(|err| {
            GhCliError::UnexpectedOutput(format!(
                "Failed to parse gh pr view --json comments response: {err}; raw: {raw}"
            ))
        })?;

        Ok(wrapper
            .comments
            .into_iter()
            .map(|c| PrComment {
                id: c.id,
                author: PrCommentAuthor {
                    login: c
                        .author
                        .and_then(|a| a.login)
                        .unwrap_or_else(|| "unknown".to_string()),
                },
                author_association: c.author_association,
                body: c.body,
                created_at: c.created_at.unwrap_or_else(Utc::now),
                url: c.url,
            })
            .collect())
    }

    fn parse_pr_review_comments(raw: &str) -> Result<Vec<PrReviewComment>, GhCliError> {
        let items: Vec<GhReviewCommentResponse> =
            serde_json::from_str(raw.trim()).map_err(|err| {
                GhCliError::UnexpectedOutput(format!(
                    "Failed to parse review comments API response: {err}; raw: {raw}"
                ))
            })?;

        Ok(items
            .into_iter()
            .map(|c| PrReviewComment {
                id: c.id,
                user: ReviewCommentUser {
                    login: c
                        .user
                        .and_then(|u| u.login)
                        .unwrap_or_else(|| "unknown".to_string()),
                },
                body: c.body,
                created_at: c.created_at.unwrap_or_else(Utc::now),
                html_url: c.html_url,
                path: c.path,
                line: c.line,
                side: c.side,
                diff_hunk: c.diff_hunk,
                author_association: c.author_association,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_rollup(json: &str) -> Vec<GhCheckElement> {
        serde_json::from_str(json).expect("rollup fixture should deserialize")
    }

    #[test]
    fn empty_rollup_is_no_checks() {
        assert_eq!(rollup_check_status(&[]), CheckStatus::NoChecks);
    }

    #[test]
    fn status_context_states_classify() {
        // StatusContext carries `state`, no `conclusion`.
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"StatusContext","context":"CodeRabbit","state":"SUCCESS"}]"#
            )),
            CheckStatus::Passing
        );
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"StatusContext","context":"ci","state":"PENDING"}]"#
            )),
            CheckStatus::Pending
        );
        // The false-green guard: a failing StatusContext must NOT be misread as
        // neutral just because it lacks a `conclusion` field.
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"StatusContext","context":"ci","state":"FAILURE"}]"#
            )),
            CheckStatus::Failing
        );
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"StatusContext","context":"ci","state":"ERROR"}]"#
            )),
            CheckStatus::Failing
        );
    }

    #[test]
    fn check_run_status_and_conclusion_classify() {
        // In-progress CheckRun (conclusion null) → pending, not neutral.
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"CheckRun","name":"build","status":"IN_PROGRESS","conclusion":null}]"#
            )),
            CheckStatus::Pending
        );
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"CheckRun","name":"build","status":"QUEUED","conclusion":null}]"#
            )),
            CheckStatus::Pending
        );
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"SUCCESS"}]"#
            )),
            CheckStatus::Passing
        );
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":"FAILURE"}]"#
            )),
            CheckStatus::Failing
        );
        // COMPLETED with a missing conclusion must not be guessed green.
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"CheckRun","name":"build","status":"COMPLETED","conclusion":null}]"#
            )),
            CheckStatus::Pending
        );
    }

    #[test]
    fn non_success_conclusions_are_failing() {
        for c in [
            "TIMED_OUT",
            "CANCELLED",
            "ACTION_REQUIRED",
            "STARTUP_FAILURE",
            "STALE",
        ] {
            let json = format!(
                r#"[{{"__typename":"CheckRun","name":"x","status":"COMPLETED","conclusion":"{c}"}}]"#
            );
            assert_eq!(
                rollup_check_status(&parse_rollup(&json)),
                CheckStatus::Failing,
                "conclusion {c} should be failing"
            );
        }
    }

    #[test]
    fn only_skipped_or_neutral_is_no_checks() {
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"CheckRun","name":"x","status":"COMPLETED","conclusion":"SKIPPED"},
                    {"__typename":"CheckRun","name":"y","status":"COMPLETED","conclusion":"NEUTRAL"}]"#
            )),
            CheckStatus::NoChecks
        );
    }

    #[test]
    fn failing_dominates_pending_and_passing() {
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"CheckRun","name":"a","status":"COMPLETED","conclusion":"SUCCESS"},
                    {"__typename":"CheckRun","name":"b","status":"IN_PROGRESS","conclusion":null},
                    {"__typename":"CheckRun","name":"c","status":"COMPLETED","conclusion":"FAILURE"}]"#
            )),
            CheckStatus::Failing
        );
    }

    #[test]
    fn pending_dominates_passing() {
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"StatusContext","context":"a","state":"SUCCESS"},
                    {"__typename":"CheckRun","name":"b","status":"IN_PROGRESS","conclusion":null}]"#
            )),
            CheckStatus::Pending
        );
    }

    #[test]
    fn skipped_alongside_success_is_passing() {
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"CheckRun","name":"a","status":"COMPLETED","conclusion":"SKIPPED"},
                    {"__typename":"CheckRun","name":"b","status":"COMPLETED","conclusion":"SUCCESS"}]"#
            )),
            CheckStatus::Passing
        );
    }

    #[test]
    fn unknown_typename_is_neutral_never_pass_or_fail() {
        // A future/unrecognized rollup element must not read as pass or fail.
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"MergeQueueEntry","state":"QUEUED"}]"#
            )),
            CheckStatus::NoChecks
        );
        // …but it must not mask a real failure either.
        assert_eq!(
            rollup_check_status(&parse_rollup(
                r#"[{"__typename":"MergeQueueEntry","state":"QUEUED"},
                    {"__typename":"StatusContext","context":"ci","state":"FAILURE"}]"#
            )),
            CheckStatus::Failing
        );
    }
}
