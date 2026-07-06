use std::time::Duration;

use backon::{ExponentialBuilder, Retryable};
use serde_json::{Value, json};

use crate::{
    error::LinearError,
    types::{ImportedIssue, ResolvedIssue, WorkflowState},
};

const LINEAR_API_URL: &str = "https://api.linear.app/graphql";
/// Whole-request budget. A stalled Linear call must produce a bounded (and
/// retryable) timeout rather than pinning the outbound sync task forever.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Page size for the inbound import sweep (Linear caps `first` at 250).
const IMPORT_PAGE_SIZE: u32 = 50;
/// Hard cap on pages walked per sweep. With `orderBy: updatedAt` (newest first)
/// and a `updatedAt` window, hitting this means the recent-activity set is huge;
/// we warn rather than scan unboundedly. 40 * 50 = 2000 issues per account.
const IMPORT_MAX_PAGES: u32 = 40;
/// Linear workflow-state categories that represent finished work. Excluded from
/// the import sweep so a first run does not pull the entire closed backlog in as
/// intake cards (JM-734 review, DA #2 / Codex backlog finding).
const TERMINAL_STATE_TYPES: [&str; 2] = ["completed", "canceled"];

/// A single-account Linear GraphQL client. Holds the account's precomputed
/// `Authorization` header value and a shared `reqwest::Client`. Outbound-only
/// surface (v1): push issue state, read workflow states for the mapping UI,
/// resolve an identifier for manual linking.
#[derive(Clone)]
pub struct LinearClient {
    http: reqwest::Client,
    auth_header: String,
}

impl LinearClient {
    pub fn new(token: impl AsRef<str>) -> Self {
        // Timeouts turn a hung connection into a retryable transport error
        // instead of an unbounded await. Builder only fails on TLS-backend init,
        // which won't happen here; fall back to an untimed default if it does.
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            auth_header: authorization_header(token.as_ref()),
        }
    }

    /// Push a new workflow state onto a Linear issue. Errors if Linear reports
    /// `success: false` or the issue/state is invalid.
    pub async fn update_issue_state(
        &self,
        issue_id: &str,
        state_id: &str,
    ) -> Result<(), LinearError> {
        const Q: &str = "mutation($id:String!,$stateId:String!){ \
            issueUpdate(id:$id, input:{stateId:$stateId}){ success } }";
        let data = self
            .execute(Q, json!({ "id": issue_id, "stateId": state_id }))
            .await?;
        let success = data
            .pointer("/issueUpdate/success")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if success {
            Ok(())
        } else {
            Err(LinearError::Api(format!(
                "issueUpdate(id={issue_id}) returned success=false"
            )))
        }
    }

    /// List a team's workflow states (board-column targets for the state map).
    pub async fn list_workflow_states(
        &self,
        team_id: &str,
    ) -> Result<Vec<WorkflowState>, LinearError> {
        const Q: &str = "query($teamId:ID!){ \
            workflowStates(filter:{ team:{ id:{ eq:$teamId } } }){ \
            nodes { id name type position } } }";
        let data = self.execute(Q, json!({ "teamId": team_id })).await?;
        let nodes = data
            .pointer("/workflowStates/nodes")
            .cloned()
            .ok_or_else(|| LinearError::Malformed("missing workflowStates.nodes".into()))?;
        serde_json::from_value(nodes)
            .map_err(|e| LinearError::Malformed(format!("workflowStates.nodes: {e}")))
    }

    /// Resolve a Linear issue by team-key identifier (e.g. `OOM-123`) for the
    /// manual-link flow. `NotFound` if the identifier is malformed or no such
    /// issue exists.
    ///
    /// The `issue(id:)` GraphQL field only accepts a UUID, so a human identifier
    /// must be resolved by filtering on the team key + issue number instead.
    pub async fn resolve_issue_by_identifier(
        &self,
        identifier: &str,
    ) -> Result<ResolvedIssue, LinearError> {
        let (team_key, number) = split_identifier(identifier)
            .ok_or_else(|| LinearError::NotFound(identifier.to_string()))?;
        const Q: &str = "query($key:String!,$number:Float!){ \
            issues(filter:{ team:{ key:{ eq:$key } }, number:{ eq:$number } }){ \
            nodes { id identifier url state { id } team { id } } } }";
        let data = self
            .execute(Q, json!({ "key": team_key, "number": number }))
            .await?;
        let issue = data
            .pointer("/issues/nodes/0")
            .filter(|v| !v.is_null())
            .ok_or_else(|| LinearError::NotFound(identifier.to_string()))?;
        parse_resolved_issue(issue)
    }

    /// Import sweep (JM-734 inbound): list issues in `team_id` that are either
    /// assigned to the token's own user OR carry `label` (when set), updated
    /// since `updated_after` (RFC3339), excluding terminal states. Walks Linear's
    /// Relay pagination newest-first so a page-cap hit never starves recent work;
    /// each page independently retries on transient errors via `execute`.
    pub async fn list_assigned_issues(
        &self,
        team_id: &str,
        label: Option<&str>,
        updated_after: &str,
    ) -> Result<Vec<ImportedIssue>, LinearError> {
        const Q: &str = "query($filter:IssueFilter!,$first:Int!,$after:String){ \
            issues(filter:$filter, first:$first, after:$after, orderBy:updatedAt){ \
            nodes { id identifier title url state { id } } \
            pageInfo { hasNextPage endCursor } } }";
        let filter = build_import_filter(team_id, label, updated_after);
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..IMPORT_MAX_PAGES {
            let vars = json!({ "filter": filter, "first": IMPORT_PAGE_SIZE, "after": after });
            let data = self.execute(Q, vars).await?;
            let nodes = data
                .pointer("/issues/nodes")
                .ok_or_else(|| LinearError::Malformed("missing issues.nodes".into()))?;
            out.extend(parse_imported_issues(nodes)?);
            let has_next = data
                .pointer("/issues/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let cursor = data
                .pointer("/issues/pageInfo/endCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            match (has_next, cursor) {
                (true, Some(c)) => after = Some(c),
                _ => return Ok(out),
            }
        }
        tracing::warn!(
            "list_assigned_issues hit the {IMPORT_MAX_PAGES}-page cap for team {team_id}; \
             import may be incomplete (oldest recent issues skipped this sweep)"
        );
        Ok(out)
    }

    /// Send a GraphQL request with `backon` retry on transient failures, then
    /// hand the raw (status, body-text) to `classify`, returning the `data`
    /// object. The body is read as text (not JSON) so a non-JSON transient
    /// (empty/HTML 429/5xx) is still classified retryable rather than terminal.
    async fn execute(&self, query: &str, variables: Value) -> Result<Value, LinearError> {
        let payload = json!({ "query": query, "variables": variables });
        (|| async {
            let resp = self
                .http
                .post(LINEAR_API_URL)
                .header("Authorization", &self.auth_header)
                .header("Content-Type", "application/json")
                .json(&payload)
                .send()
                .await
                .map_err(|e| LinearError::Http(e.to_string()))?;
            let status = resp.status().as_u16();
            let body = resp
                .text()
                .await
                .map_err(|e| LinearError::Http(e.to_string()))?;
            classify(status, &body)
        })
        .retry(
            &ExponentialBuilder::default()
                .with_min_delay(Duration::from_secs(1))
                .with_max_delay(Duration::from_secs(30))
                .with_max_times(3)
                .with_jitter(),
        )
        .when(|e: &LinearError| e.should_retry())
        .notify(|err: &LinearError, dur: Duration| {
            tracing::warn!(
                "Linear API call failed, retrying after {:.2}s: {}",
                dur.as_secs_f64(),
                err
            );
        })
        .await
    }
}

/// Format the `Authorization` header for a Linear credential. Personal API keys
/// (prefix `lin_api_`) are sent verbatim; OAuth access tokens require the
/// `Bearer` scheme. A value that already carries a scheme is passed through.
/// Getting this wrong 401s an entire credential type, so it is a pure, tested fn.
fn authorization_header(token: &str) -> String {
    let t = token.trim();
    let lower = t.to_ascii_lowercase();
    if t.starts_with("lin_api_") || lower.starts_with("bearer ") {
        t.to_string()
    } else {
        format!("Bearer {t}")
    }
}

/// Map a raw GraphQL (HTTP status, body text) into either the `data` object or
/// a classified `LinearError`. Pure — unit-tested directly, no network.
///
/// Precedence: retryable/auth HTTP statuses are decided from the status code
/// alone (no JSON required, so a non-JSON 429/5xx stays retryable); only on a
/// 2xx is the body parsed, then a non-empty GraphQL `errors` array is inspected
/// (`extensions.type` + message), then the `data` object is returned.
fn classify(status: u16, body: &str) -> Result<Value, LinearError> {
    match status {
        429 => return Err(LinearError::RateLimited),
        401 | 403 => {
            return Err(LinearError::AuthFailed(
                json_error_message(body).unwrap_or_else(|| format!("HTTP {status}")),
            ));
        }
        s if (500..600).contains(&s) => {
            // Transient server-side — retryable regardless of body shape.
            return Err(LinearError::Http(
                json_error_message(body).unwrap_or_else(|| format!("HTTP {s}")),
            ));
        }
        s if !(200..300).contains(&s) => {
            // Other 4xx: terminal.
            return Err(LinearError::Api(
                json_error_message(body).unwrap_or_else(|| format!("HTTP {s}")),
            ));
        }
        _ => {}
    }

    // 2xx: the body must be JSON now — a malformed *successful* response is a
    // genuine (terminal) protocol error, not a transient one.
    let parsed: Value = serde_json::from_str(body)
        .map_err(|e| LinearError::Malformed(format!("2xx body not JSON: {e}")))?;

    if let Some(errors) = parsed.get("errors").and_then(Value::as_array)
        && !errors.is_empty()
    {
        let first = &errors[0];
        let kind = first
            .pointer("/extensions/type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let msg = first
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown GraphQL error")
            .to_string();
        return Err(match kind.as_str() {
            "ratelimited" => LinearError::RateLimited,
            "authentication" | "authorization" => LinearError::AuthFailed(msg),
            _ if is_not_found(&kind, &msg) => LinearError::NotFound(msg),
            _ => LinearError::Api(msg),
        });
    }

    parsed
        .get("data")
        .filter(|v| !v.is_null())
        .cloned()
        .ok_or_else(|| LinearError::Malformed("response had neither data nor errors".into()))
}

fn is_not_found(kind: &str, msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    kind.contains("not found")
        || kind.contains("invalid input")
        || m.contains("entity not found")
        || m.contains("could not find")
}

/// Best-effort first GraphQL error message from a (possibly non-JSON) body.
fn json_error_message(body: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    parsed
        .get("errors")?
        .as_array()?
        .first()?
        .get("message")?
        .as_str()
        .map(str::to_string)
}

/// Split a Linear issue identifier like `OOM-123` into its team key and issue
/// number. Returns `None` for anything that is not `<KEY>-<NUMBER>`. Pure — the
/// `issue(id:)` GraphQL field only takes a UUID, so the manual-link flow resolves
/// the human identifier via team key + number, which this parses out.
fn split_identifier(identifier: &str) -> Option<(&str, u64)> {
    let (key, number) = identifier.trim().rsplit_once('-')?;
    if key.is_empty() {
        return None;
    }
    let number: u64 = number.parse().ok()?;
    Some((key, number))
}

/// Build the Linear `IssueFilter` for the import sweep. Pure so the filter shape
/// (the one runtime dependency unit tests can't otherwise exercise) is at least
/// structurally pinned. Scopes to the team, excludes terminal states, windows by
/// `updatedAt`, and requires assigned-to-me OR the configured label.
///
/// A malformed filter 400s the entire sweep, so the shape mirrors Linear's
/// documented `IssueFilter`: `team.id.eq`, `state.type.nin`, `updatedAt.gt`,
/// `assignee.isMe.eq`, `labels.some.name.eq`, and top-level `and`/`or`.
fn build_import_filter(team_id: &str, label: Option<&str>, updated_after: &str) -> Value {
    let assigned = json!({ "assignee": { "isMe": { "eq": true } } });
    // Assigned-to-me OR labelled, when a label filter is configured.
    let who = match label {
        Some(l) => json!({ "or": [assigned, { "labels": { "some": { "name": { "eq": l } } } }] }),
        None => assigned,
    };
    json!({
        "and": [
            { "team": { "id": { "eq": team_id } } },
            { "state": { "type": { "nin": TERMINAL_STATE_TYPES } } },
            { "updatedAt": { "gt": updated_after } },
            who,
        ]
    })
}

/// Parse the `issues.nodes` array of the import sweep into `ImportedIssue`s.
/// Split out for unit testing without a live GraphQL round-trip. A node missing
/// a required scalar (`id`/`identifier`/`title`/`url`) is a protocol error
/// (terminal `Malformed`), not silently dropped.
fn parse_imported_issues(nodes: &Value) -> Result<Vec<ImportedIssue>, LinearError> {
    let arr = nodes
        .as_array()
        .ok_or_else(|| LinearError::Malformed("issues.nodes not an array".into()))?;
    arr.iter()
        .map(|n| {
            let field = |name: &str| {
                n.get(name)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| LinearError::Malformed(format!("issue.{name} missing")))
            };
            Ok(ImportedIssue {
                id: field("id")?,
                identifier: field("identifier")?,
                title: field("title")?,
                url: field("url")?,
                state_id: n
                    .pointer("/state/id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

/// Parse the `issue { ... }` sub-object of a resolve query into `ResolvedIssue`.
/// Split out so it is unit-testable without a live GraphQL round-trip.
fn parse_resolved_issue(issue: &Value) -> Result<ResolvedIssue, LinearError> {
    let field = |name: &str| {
        issue
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| LinearError::Malformed(format!("issue.{name} missing")))
    };
    Ok(ResolvedIssue {
        id: field("id")?,
        identifier: field("identifier")?,
        url: field("url")?,
        state_id: issue
            .pointer("/state/id")
            .and_then(Value::as_str)
            .map(str::to_string),
        team_id: issue
            .pointer("/team/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| LinearError::Malformed("issue.team.id missing".into()))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personal_api_key_sent_verbatim() {
        assert_eq!(authorization_header("lin_api_abc123"), "lin_api_abc123");
    }

    #[test]
    fn oauth_token_gets_bearer_scheme() {
        assert_eq!(
            authorization_header("oauth_opaque_xyz"),
            "Bearer oauth_opaque_xyz"
        );
    }

    #[test]
    fn already_bearer_passed_through() {
        assert_eq!(authorization_header("Bearer tok"), "Bearer tok");
        assert_eq!(authorization_header("  bearer tok  "), "bearer tok");
    }

    #[test]
    fn classify_rate_limited_by_http_status() {
        let err = classify(429, "").unwrap_err();
        assert!(matches!(err, LinearError::RateLimited));
        assert!(err.should_retry());
    }

    #[test]
    fn classify_429_with_html_body_still_retryable() {
        // Regression: a non-JSON transient must not become terminal Malformed.
        let err = classify(429, "<html>rate limited</html>").unwrap_err();
        assert!(matches!(err, LinearError::RateLimited));
        assert!(err.should_retry());
    }

    #[test]
    fn classify_5xx_with_empty_body_is_retryable_http() {
        let err = classify(503, "").unwrap_err();
        assert!(matches!(err, LinearError::Http(_)));
        assert!(err.should_retry());
    }

    #[test]
    fn classify_auth_failed_by_http_status() {
        let err = classify(401, r#"{"errors":[{"message":"bad token"}]}"#).unwrap_err();
        assert!(matches!(err, LinearError::AuthFailed(_)));
        assert!(!err.should_retry());
    }

    #[test]
    fn classify_graphql_ratelimited_extension() {
        let body = r#"{"errors":[{"message":"slow down","extensions":{"type":"ratelimited"}}]}"#;
        assert!(matches!(
            classify(200, body).unwrap_err(),
            LinearError::RateLimited
        ));
    }

    #[test]
    fn classify_graphql_not_found() {
        let body =
            r#"{"errors":[{"message":"Entity not found","extensions":{"type":"invalid input"}}]}"#;
        let err = classify(200, body).unwrap_err();
        assert!(matches!(err, LinearError::NotFound(_)));
        assert!(!err.should_retry());
    }

    #[test]
    fn classify_returns_data_on_success() {
        let body = r#"{"data":{"issueUpdate":{"success":true}}}"#;
        let data = classify(200, body).unwrap();
        assert_eq!(data.pointer("/issueUpdate/success").unwrap(), &json!(true));
    }

    #[test]
    fn classify_2xx_non_json_is_terminal_malformed() {
        // A 200 with a non-JSON body IS terminal — retrying won't help.
        let err = classify(200, "<html>ok?</html>").unwrap_err();
        assert!(matches!(err, LinearError::Malformed(_)));
        assert!(!err.should_retry());
    }

    #[test]
    fn classify_generic_graphql_error_is_terminal_api() {
        let body = r#"{"errors":[{"message":"state not on team"}]}"#;
        let err = classify(200, body).unwrap_err();
        assert!(matches!(err, LinearError::Api(_)));
        assert!(!err.should_retry());
    }

    #[test]
    fn split_identifier_valid() {
        assert_eq!(split_identifier("OOM-123"), Some(("OOM", 123)));
        assert_eq!(split_identifier("  JM-7  "), Some(("JM", 7)));
    }

    #[test]
    fn split_identifier_rejects_malformed() {
        // No dash, empty key, empty number, and non-numeric number all reject.
        assert_eq!(split_identifier("garbage"), None);
        assert_eq!(split_identifier("-5"), None);
        assert_eq!(split_identifier("OOM-"), None);
        assert_eq!(split_identifier("OOM-abc"), None);
    }

    #[test]
    fn parse_resolved_issue_full() {
        let issue = json!({
            "id": "uuid-1", "identifier": "OOM-123", "url": "https://linear.app/x/OOM-123",
            "state": { "id": "state-uuid" }, "team": { "id": "team-uuid" }
        });
        let r = parse_resolved_issue(&issue).unwrap();
        assert_eq!(r.id, "uuid-1");
        assert_eq!(r.identifier, "OOM-123");
        assert_eq!(r.state_id.as_deref(), Some("state-uuid"));
        assert_eq!(r.team_id, "team-uuid");
    }

    #[test]
    fn parse_resolved_issue_null_state_ok() {
        let issue = json!({
            "id": "uuid-1", "identifier": "OOM-1", "url": "u",
            "state": null, "team": { "id": "team-uuid" }
        });
        let r = parse_resolved_issue(&issue).unwrap();
        assert_eq!(r.state_id, None);
    }

    #[test]
    fn parse_resolved_issue_missing_team_is_malformed() {
        let issue = json!({ "id": "u", "identifier": "O-1", "url": "u", "state": null });
        assert!(matches!(
            parse_resolved_issue(&issue).unwrap_err(),
            LinearError::Malformed(_)
        ));
    }

    #[test]
    fn workflow_state_deserializes() {
        let node = json!({ "id": "s1", "name": "In Progress", "type": "started", "position": 2.0 });
        let ws: WorkflowState = serde_json::from_value(node).unwrap();
        assert_eq!(ws.name, "In Progress");
        assert_eq!(ws.state_type, "started");
    }

    #[test]
    fn import_filter_without_label_is_assignee_only() {
        let f = build_import_filter("team-1", None, "2026-04-01T00:00:00Z");
        let and = f.get("and").and_then(Value::as_array).unwrap();
        // team scope, terminal exclusion, updatedAt window, assignee clause.
        assert_eq!(and.len(), 4);
        assert_eq!(and[0].pointer("/team/id/eq").unwrap(), "team-1");
        assert_eq!(
            and[1].pointer("/state/type/nin").unwrap(),
            &json!(["completed", "canceled"])
        );
        assert_eq!(
            and[2].pointer("/updatedAt/gt").unwrap(),
            "2026-04-01T00:00:00Z"
        );
        // No label → the who-clause is the bare assignee filter, not an `or`.
        assert_eq!(and[3].pointer("/assignee/isMe/eq").unwrap(), &json!(true));
        assert!(and[3].get("or").is_none());
    }

    #[test]
    fn import_filter_with_label_is_assignee_or_label() {
        let f = build_import_filter("team-1", Some("agent-eligible"), "2026-04-01T00:00:00Z");
        let who = &f.get("and").and_then(Value::as_array).unwrap()[3];
        let or = who.get("or").and_then(Value::as_array).unwrap();
        assert_eq!(or.len(), 2);
        assert_eq!(or[0].pointer("/assignee/isMe/eq").unwrap(), &json!(true));
        assert_eq!(
            or[1].pointer("/labels/some/name/eq").unwrap(),
            "agent-eligible"
        );
    }

    #[test]
    fn parse_imported_issues_full_and_null_state() {
        let nodes = json!([
            { "id": "u1", "identifier": "JM-1", "title": "First", "url": "https://l/JM-1",
              "state": { "id": "s1" } },
            { "id": "u2", "identifier": "JM-2", "title": "Second", "url": "https://l/JM-2",
              "state": null },
        ]);
        let out = parse_imported_issues(&nodes).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "First");
        assert_eq!(out[0].state_id.as_deref(), Some("s1"));
        assert_eq!(out[1].identifier, "JM-2");
        assert_eq!(out[1].state_id, None);
    }

    #[test]
    fn parse_imported_issues_empty_is_ok() {
        assert!(parse_imported_issues(&json!([])).unwrap().is_empty());
    }

    #[test]
    fn parse_imported_issues_missing_title_is_malformed() {
        // A required scalar missing is a terminal protocol error, not a drop.
        let nodes = json!([{ "id": "u1", "identifier": "JM-1", "url": "u", "state": null }]);
        assert!(matches!(
            parse_imported_issues(&nodes).unwrap_err(),
            LinearError::Malformed(_)
        ));
    }

    #[test]
    fn parse_imported_issues_non_array_is_malformed() {
        assert!(matches!(
            parse_imported_issues(&json!({ "not": "an array" })).unwrap_err(),
            LinearError::Malformed(_)
        ));
    }
}
