use serde::Deserialize;

/// A Linear workflow state (board column target). Team-scoped. Returned by
/// `list_workflow_states` to populate the state-map editor.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    /// Linear state category: `backlog` | `unstarted` | `started` | `completed`
    /// | `canceled` | `triage`. Kept as a string — the mapping UI groups by it.
    #[serde(rename = "type")]
    pub state_type: String,
    /// Ordering hint within the team's workflow.
    #[serde(default)]
    pub position: f64,
}

/// The subset of a Linear issue needed to import it as a kanban card (JM-734
/// inbound sweep). Distinct from `ResolvedIssue` (manual link) because import
/// carries the `title` for the new card and does not need the owning team id
/// (the sweep already queried a single team).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedIssue {
    /// Globally-unique issue UUID (stored as `issues.linear_issue_id`).
    pub id: String,
    /// Team-key identifier, e.g. `OOM-123` (stored as `linear_issue_identifier`).
    pub identifier: String,
    /// Issue title — becomes the imported card's title.
    pub title: String,
    /// Web URL to the issue (stored as `linear_url`).
    pub url: String,
    /// Current workflow-state id, if any. Seeds `linear_state_id` (outbound
    /// drift baseline) and drives the reverse state-map resolution.
    pub state_id: Option<String>,
}

/// The subset of a Linear issue needed to establish a manual card link.
#[derive(Debug, Clone)]
pub struct ResolvedIssue {
    /// Globally-unique issue UUID (stored as `issues.linear_issue_id`).
    pub id: String,
    /// Team-key identifier, e.g. `OOM-123` (stored as `linear_issue_identifier`).
    pub identifier: String,
    /// Web URL to the issue (stored as `linear_url`).
    pub url: String,
    /// Current workflow-state id, if any (seeds `linear_state_id` for drift).
    pub state_id: Option<String>,
    /// Owning team id — lets the caller confirm the issue belongs to the
    /// account's configured team before linking.
    pub team_id: String,
}
