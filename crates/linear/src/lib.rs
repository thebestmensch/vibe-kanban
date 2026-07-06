//! Concrete Linear GraphQL client for the outbound card→ticket status mirror
//! (JM-718). Deliberately Linear-only and outbound-only in v1 — no `IssueHost`
//! trait abstraction, no inbound import (that is JM-734). See ADR 0002.

mod client;
mod error;
mod types;

use std::collections::HashMap;

pub use client::LinearClient;
pub use error::LinearError;
pub use types::{ImportedIssue, ResolvedIssue, WorkflowState};

/// A keyed collection of per-account Linear clients (multi-account from day one,
/// e.g. `"personal"` + `"work"`). Callers resolve a client by the account key
/// bound to a project (`projects.linear_account_key`).
#[derive(Clone, Default)]
pub struct LinearService {
    accounts: HashMap<String, LinearClient>,
}

impl LinearService {
    pub fn new(accounts: HashMap<String, LinearClient>) -> Self {
        Self { accounts }
    }

    /// Build from a map of `account_key -> token`.
    pub fn from_tokens(tokens: HashMap<String, String>) -> Self {
        Self {
            accounts: tokens
                .into_iter()
                .map(|(k, token)| (k, LinearClient::new(token)))
                .collect(),
        }
    }

    /// Resolve the client for an account key, or `UnknownAccount`.
    pub fn client(&self, account_key: &str) -> Result<&LinearClient, LinearError> {
        self.accounts
            .get(account_key)
            .ok_or_else(|| LinearError::UnknownAccount(account_key.to_string()))
    }

    /// Whether an account key is configured.
    pub fn has_account(&self, account_key: &str) -> bool {
        self.accounts.contains_key(account_key)
    }
}
