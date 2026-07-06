//! Linear status mirror. Outbound (JM-718): board owns status; a column-move on
//! a linked card pushes the mapped Linear workflow-state. Inbound (JM-734):
//! assigned/labelled Linear issues import as cards in a per-account target
//! project. Cloned shell of `pr_monitor` (interval + `Notify` hybrid).
//!
//! Echo invariant (ADR 0002 §8): `linear_sync_pending = 1` is set ONLY by the
//! user-driven mutation hook in `Issues::update_on`. Both arms' own writes use
//! dedicated non-flagging model methods (`Issues::import_from_linear` writes
//! `pending = 0`), so an imported card never bounces back out to Linear.
//!
//! Inbound runs on the interval tick ONLY, never on the `sync_notify` nudge —
//! that nudge fires on every card move, and coupling a full paginated import
//! sweep to it would turn each move into a network storm and delay the very
//! outbound push the move triggered (JM-734 design review).

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::Utc;
use db::{
    DBService,
    models::board::{
        ImportCard, Issues, PendingLinearSync, ProjectStatuses, resolve_import_status,
    },
};
use linear::{LinearClient, LinearError};
use tokio::{
    sync::{Notify, RwLock},
    time::interval,
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::services::config::{Config, LinearAccount};

/// How far back the inbound sweep looks. Bounds a large assignee backlog to a
/// durable window so a page-cap hit (newest-first) never starves recent work,
/// and a first run doesn't drag in years of stale-but-active assignments.
const IMPORT_LOOKBACK_DAYS: i64 = 90;

/// Resolved outcome for one pending card, decided under a short-lived config
/// read guard so the (slow) network push never holds the lock.
enum Action {
    /// Push `state_id` to Linear for a fully-resolved, mapped, linked card.
    /// `status_id` is the snapshotted column — it guards the clear against a
    /// concurrent move (see `Issues::mark_linear_synced`).
    Push {
        id: uuid::Uuid,
        status_id: uuid::Uuid,
        linear_issue_id: String,
        state_id: String,
        token: String,
    },
    /// Deterministic non-syncable state — clear the flag so it stops spinning.
    /// `status_id` guards the clear the same way.
    Clear {
        id: uuid::Uuid,
        status_id: uuid::Uuid,
        reason: &'static str,
    },
    /// Recoverable once config/creds change — leave the flag set for a retry.
    Leave { id: uuid::Uuid, reason: String },
}

pub struct LinearMonitorService {
    db: DBService,
    config: Arc<RwLock<Config>>,
    poll_interval: Duration,
    sync_notify: Arc<Notify>,
}

impl LinearMonitorService {
    pub async fn spawn(
        db: DBService,
        config: Arc<RwLock<Config>>,
        sync_notify: Arc<Notify>,
    ) -> tokio::task::JoinHandle<()> {
        let service = Self {
            db,
            config,
            poll_interval: Duration::from_secs(60),
            sync_notify,
        };
        tokio::spawn(async move {
            service.start().await;
        })
    }

    async fn start(&self) {
        info!(
            "Starting Linear sync service (outbound + inbound) with interval {:?}",
            self.poll_interval
        );
        let mut interval = interval(self.poll_interval);
        loop {
            // Inbound import runs on the interval tick only. A `sync_notify` nudge
            // fires on every card move and must drive the outbound drain *only* —
            // never a paginated Linear import.
            let do_inbound = tokio::select! {
                _ = interval.tick() => true,
                _ = self.sync_notify.notified() => {
                    debug!("Linear sync triggered externally");
                    false
                }
            };
            // Outbound first so a nudge-triggered push isn't queued behind a slow
            // inbound sweep.
            self.drain_pending().await;
            if do_inbound {
                self.sweep_inbound().await;
            }
        }
    }

    /// Push every pending, linked card to Linear. Config-dependent resolution
    /// happens under a brief read guard; all network I/O runs after the guard is
    /// dropped so a stalled push can't block config writes.
    async fn drain_pending(&self) {
        let pending = match Issues::list_pending_linear_sync(&self.db.pool).await {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to query pending Linear syncs: {e}");
                return;
            }
        };
        if pending.is_empty() {
            return;
        }
        debug!("Draining {} pending Linear sync(s)", pending.len());

        let actions: Vec<Action> = {
            let cfg = self.config.read().await;
            pending.iter().map(|item| resolve(&cfg, item)).collect()
        };

        for action in actions {
            match action {
                Action::Clear {
                    id,
                    status_id,
                    reason,
                } => {
                    warn!(issue = %id, reason, "skipping Linear push; clearing pending");
                    if let Err(e) = Issues::clear_linear_pending(&self.db.pool, id, status_id).await
                    {
                        error!(issue = %id, "failed to clear Linear pending flag: {e}");
                    }
                }
                Action::Leave { id, reason } => {
                    warn!(issue = %id, reason, "leaving card pending for a later retry");
                }
                Action::Push {
                    id,
                    status_id,
                    linear_issue_id,
                    state_id,
                    token,
                } => {
                    self.push_one(id, status_id, &linear_issue_id, &state_id, &token)
                        .await;
                }
            }
        }
    }

    /// Push one resolved card and reconcile the outcome. Terminal successes and
    /// deterministic failures clear the flag; transient/recoverable errors leave
    /// it for the next tick; a Linear-side deletion unlinks the card.
    async fn push_one(
        &self,
        id: uuid::Uuid,
        status_id: uuid::Uuid,
        linear_issue_id: &str,
        state_id: &str,
        token: &str,
    ) {
        let client = LinearClient::new(token);
        match client.update_issue_state(linear_issue_id, state_id).await {
            Ok(()) => {
                match Issues::mark_linear_synced(&self.db.pool, id, status_id, state_id).await {
                    Ok(()) => info!(issue = %id, "pushed card status to Linear"),
                    Err(e) => error!(
                        issue = %id,
                        "pushed to Linear but failed to record synced state: {e}"
                    ),
                }
            }
            Err(LinearError::NotFound(_)) => {
                warn!(issue = %id, linear = %linear_issue_id, "Linear issue not found; unlinking card");
                if let Err(e) = Issues::unlink_linear(&self.db.pool, id).await {
                    error!(issue = %id, "failed to unlink card: {e}");
                }
            }
            Err(e) if e.should_retry() => {
                warn!(issue = %id, "transient Linear error; leaving pending for next tick: {e}");
            }
            Err(e) => {
                // Everything else — auth, unknown-account, `Api` (e.g. an invalid
                // or since-deleted mapped state id), `Malformed` — is left
                // pending, NOT cleared. Clearing a *failed* push would silently
                // drop the sync: the move would look processed while Linear never
                // received it (a state-map typo is the common trigger). Leaving
                // it means the card stays visibly un-synced (drift badge + logs)
                // and recovers once the mapping/creds are fixed. Only a *success*
                // clears pending; only a Linear-side deletion (NotFound) unlinks.
                warn!(
                    issue = %id,
                    "Linear push failed; leaving pending until config/creds fixed: {e}"
                );
            }
        }
    }

    /// Inbound sweep (JM-734): for each account configured for import, pull
    /// assigned/labelled Linear issues and create cards in its target project.
    /// Config-dependent decisions happen under a brief read guard; all network +
    /// DB I/O runs after the guard drops. Per-account failures are logged and
    /// isolated — one bad token never aborts the sweep or the other account.
    async fn sweep_inbound(&self) {
        let plans: Vec<ImportPlan> = {
            let cfg = self.config.read().await;
            cfg.linear
                .accounts
                .iter()
                .filter_map(|(key, account)| plan_import(key, account))
                .collect()
        };
        if plans.is_empty() {
            return;
        }

        let updated_after =
            (Utc::now() - chrono::Duration::days(IMPORT_LOOKBACK_DAYS)).to_rfc3339();

        for plan in plans {
            if let Err(e) = self.import_for_account(&plan, &updated_after).await {
                error!(account = %plan.account_key, "inbound Linear import failed: {e}");
            }
        }
    }

    /// Import one account's issues. Split out so `?` handles the per-account
    /// error path while `sweep_inbound` isolates it from the other accounts.
    async fn import_for_account(
        &self,
        plan: &ImportPlan,
        updated_after: &str,
    ) -> Result<(), anyhow::Error> {
        // Binding invariant (re-checked at poll time to survive a rebind after
        // the import target was configured): the target project MUST still be
        // bound to this account. Importing into a project bound to a *different*
        // account would make a later card move push this account's issue-id with
        // the other account's token → NotFound → the card is silently unlinked.
        let bound = db::models::board::BoardProjects::linear_account_key(
            &self.db.pool,
            plan.target_project_id,
        )
        .await?;
        if bound.as_deref() != Some(plan.account_key.as_str()) {
            warn!(
                account = %plan.account_key,
                target = %plan.target_project_id,
                bound = ?bound,
                "import target is not bound to this account; skipping (fix the binding)"
            );
            return Ok(());
        }

        let statuses =
            ProjectStatuses::list_by_project(&self.db.pool, plan.target_project_id).await?;
        if !statuses.iter().any(|s| !s.hidden) {
            warn!(
                account = %plan.account_key,
                target = %plan.target_project_id,
                "import target has no visible column to place cards in; skipping"
            );
            return Ok(());
        }

        let client = LinearClient::new(&plan.token);
        let issues = client
            .list_assigned_issues(&plan.team_id, plan.label.as_deref(), updated_after)
            .await?;
        if issues.is_empty() {
            return Ok(());
        }

        let mut cards = Vec::with_capacity(issues.len());
        for issue in &issues {
            match resolve_import_status(&statuses, &plan.state_map, issue.state_id.as_deref()) {
                Some(status_id) => cards.push(ImportCard {
                    linear_issue_id: issue.id.clone(),
                    linear_issue_identifier: issue.identifier.clone(),
                    linear_url: issue.url.clone(),
                    title: issue.title.clone(),
                    linear_state_id: issue.state_id.clone(),
                    status_id,
                }),
                None => warn!(
                    account = %plan.account_key,
                    issue = %issue.identifier,
                    "no board column to place imported issue; skipping"
                ),
            }
        }

        let inserted =
            Issues::import_from_linear(&self.db.pool, plan.target_project_id, &cards).await?;
        if inserted > 0 {
            info!(
                account = %plan.account_key,
                inserted,
                fetched = issues.len(),
                "imported Linear issues as cards"
            );
        }
        Ok(())
    }
}

/// A resolved inbound-import job for one account, owned so the config read guard
/// drops before any network/DB I/O (mirrors the outbound `Action` pattern).
#[derive(Debug, Clone)]
struct ImportPlan {
    account_key: String,
    token: String,
    team_id: String,
    target_project_id: Uuid,
    label: Option<String>,
    /// The account's outbound state map, carried so the sweep can reverse-resolve
    /// incoming Linear states to board columns without re-reading config.
    state_map: HashMap<String, String>,
}

/// Decide whether an account is configured for inbound import. Pure so the
/// decision table is unit-testable. Returns `None` (import disabled for this
/// account) when any prerequisite is missing: no token, no team, no import
/// target, or an unparseable target id.
fn plan_import(key: &str, account: &LinearAccount) -> Option<ImportPlan> {
    let token = account.token.clone()?;
    let team_id = account.team_id.clone()?;
    let target_raw = account.import_target_project_id.as_deref()?;
    let target_project_id = match Uuid::parse_str(target_raw) {
        Ok(id) => id,
        Err(_) => {
            warn!(account = %key, target = target_raw, "import_target_project_id is not a valid UUID; skipping");
            return None;
        }
    };
    Some(ImportPlan {
        account_key: key.to_string(),
        token,
        team_id,
        target_project_id,
        label: account.import_label.clone(),
        state_map: account.state_map.clone(),
    })
}

/// Resolve one pending card against the config snapshot into an [`Action`].
/// Pure (aside from the borrowed config) so the decision table is unit-testable.
fn resolve(cfg: &Config, item: &PendingLinearSync) -> Action {
    let Some(key) = item.linear_account_key.as_deref() else {
        return Action::Clear {
            id: item.id,
            status_id: item.status_id,
            reason: "project has no bound Linear account",
        };
    };
    let Some(account) = cfg.linear.accounts.get(key) else {
        return Action::Leave {
            id: item.id,
            reason: format!("unknown Linear account '{key}'"),
        };
    };
    let Some(token) = account.token.clone() else {
        return Action::Leave {
            id: item.id,
            reason: format!("Linear account '{key}' has no token"),
        };
    };
    // `state_map` is keyed by `project_statuses.id` (a stable UUID string), not
    // by the user-editable column name.
    let Some(state_id) = account.state_map.get(&item.status_id.to_string()).cloned() else {
        return Action::Clear {
            id: item.id,
            status_id: item.status_id,
            reason: "board column is not mapped to a Linear state",
        };
    };
    Action::Push {
        id: item.id,
        status_id: item.status_id,
        linear_issue_id: item.linear_issue_id.clone(),
        state_id,
        token,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use uuid::Uuid;

    use super::*;
    use crate::services::config::{LinearAccount, LinearConfig};

    fn pending(account_key: Option<&str>, status_id: Uuid) -> PendingLinearSync {
        PendingLinearSync {
            id: Uuid::from_u128(9),
            status_id,
            linear_issue_id: "issue-uuid".to_string(),
            linear_account_key: account_key.map(str::to_string),
        }
    }

    fn config_with(key: &str, account: LinearAccount) -> Config {
        Config {
            linear: LinearConfig {
                accounts: HashMap::from([(key.to_string(), account)]),
            },
            ..Default::default()
        }
    }

    #[test]
    fn unbound_project_clears() {
        let cfg = Config::default();
        let action = resolve(&cfg, &pending(None, Uuid::from_u128(1)));
        assert!(matches!(action, Action::Clear { .. }));
    }

    #[test]
    fn unknown_account_leaves() {
        let cfg = Config::default();
        let action = resolve(&cfg, &pending(Some("work"), Uuid::from_u128(1)));
        assert!(matches!(action, Action::Leave { .. }));
    }

    #[test]
    fn missing_token_leaves() {
        let cfg = config_with(
            "work",
            LinearAccount {
                token: None,
                ..Default::default()
            },
        );
        let action = resolve(&cfg, &pending(Some("work"), Uuid::from_u128(1)));
        assert!(matches!(action, Action::Leave { .. }));
    }

    #[test]
    fn unmapped_column_clears() {
        let cfg = config_with(
            "work",
            LinearAccount {
                token: Some("lin_api_x".to_string()),
                state_map: HashMap::new(),
                ..Default::default()
            },
        );
        let action = resolve(&cfg, &pending(Some("work"), Uuid::from_u128(1)));
        assert!(matches!(action, Action::Clear { .. }));
    }

    #[test]
    fn fully_mapped_pushes() {
        let status = Uuid::from_u128(42);
        let cfg = config_with(
            "work",
            LinearAccount {
                token: Some("lin_api_x".to_string()),
                state_map: HashMap::from([(status.to_string(), "linear-state-uuid".to_string())]),
                ..Default::default()
            },
        );
        let action = resolve(&cfg, &pending(Some("work"), status));
        match action {
            Action::Push {
                state_id, token, ..
            } => {
                assert_eq!(state_id, "linear-state-uuid");
                assert_eq!(token, "lin_api_x");
            }
            _ => panic!("expected Push"),
        }
    }

    // --- inbound plan_import decision table (JM-734) -------------------------

    fn import_account(
        token: Option<&str>,
        team: Option<&str>,
        target: Option<&str>,
    ) -> LinearAccount {
        LinearAccount {
            token: token.map(str::to_string),
            team_id: team.map(str::to_string),
            import_target_project_id: target.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn plan_import_requires_token_team_and_target() {
        // Missing any one prerequisite disables import for the account.
        assert!(
            plan_import(
                "work",
                &import_account(None, Some("t"), Some(&Uuid::from_u128(1).to_string()))
            )
            .is_none()
        );
        assert!(
            plan_import(
                "work",
                &import_account(Some("k"), None, Some(&Uuid::from_u128(1).to_string()))
            )
            .is_none()
        );
        assert!(plan_import("work", &import_account(Some("k"), Some("t"), None)).is_none());
    }

    #[test]
    fn plan_import_rejects_unparseable_target() {
        assert!(
            plan_import(
                "work",
                &import_account(Some("k"), Some("t"), Some("not-a-uuid"))
            )
            .is_none()
        );
    }

    #[test]
    fn plan_import_fully_configured_yields_plan() {
        let target = Uuid::from_u128(7);
        let mut acct = import_account(Some("lin_api_x"), Some("team-9"), Some(&target.to_string()));
        acct.import_label = Some("agent-eligible".to_string());
        let plan = plan_import("work", &acct).expect("configured account yields a plan");
        assert_eq!(plan.account_key, "work");
        assert_eq!(plan.token, "lin_api_x");
        assert_eq!(plan.team_id, "team-9");
        assert_eq!(plan.target_project_id, target);
        assert_eq!(plan.label.as_deref(), Some("agent-eligible"));
    }
}
