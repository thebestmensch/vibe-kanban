//! Outbound Linear status mirror (JM-718). Board owns status; a column-move on
//! a linked card pushes the mapped Linear workflow-state. Cloned shell of
//! `pr_monitor` (interval + `Notify` hybrid), outbound arm only. There is no
//! inbound writer in this ticket, so no echo path exists: `linear_sync_pending`
//! is set exclusively by the user-driven mutation hook in `Issues::update_on`,
//! and every write this loop makes uses the dedicated (non-flagging) model
//! methods. See ADR 0002.

use std::{sync::Arc, time::Duration};

use db::{
    DBService,
    models::board::{Issues, PendingLinearSync},
};
use linear::{LinearClient, LinearError};
use tokio::{
    sync::{Notify, RwLock},
    time::interval,
};
use tracing::{debug, error, info, warn};

use crate::services::config::Config;

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
            "Starting Linear outbound sync service with interval {:?}",
            self.poll_interval
        );
        let mut interval = interval(self.poll_interval);
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = self.sync_notify.notified() => {
                    debug!("Linear sync triggered externally");
                }
            }
            self.drain_pending().await;
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
            Err(e @ (LinearError::AuthFailed(_) | LinearError::UnknownAccount(_))) => {
                // Config-fixable — do NOT clear, or a bad token silently drops
                // the sync. Recovers on the next tick once creds are corrected.
                warn!(issue = %id, "Linear auth/account error; leaving pending until fixed: {e}");
            }
            Err(e) => {
                // Api | Malformed — deterministic; clear so it stops spinning.
                error!(issue = %id, "permanent Linear error; clearing pending (won't retry): {e}");
                if let Err(e) = Issues::clear_linear_pending(&self.db.pool, id, status_id).await {
                    error!(issue = %id, "failed to clear Linear pending flag: {e}");
                }
            }
        }
    }
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
}
