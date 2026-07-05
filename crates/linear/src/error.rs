use thiserror::Error;

/// Errors from the Linear GraphQL client. `should_retry` classifies which are
/// transient (worth a `backon` retry) vs terminal.
#[derive(Debug, Error)]
pub enum LinearError {
    /// Token missing/invalid or lacks permission (HTTP 401/403, or a GraphQL
    /// authentication/authorization error). Never retried.
    #[error("Linear authentication failed: {0}")]
    AuthFailed(String),

    /// HTTP 429 or a GraphQL `ratelimited` error. Retried (backon backoff).
    #[error("Linear rate limited")]
    RateLimited,

    /// The requested issue/entity does not exist. Terminal — the caller decides
    /// (the sync loop unlinks the card rather than retrying a dead issue).
    #[error("Linear entity not found: {0}")]
    NotFound(String),

    /// A GraphQL user error that is neither auth, rate-limit, nor not-found
    /// (e.g. a mutation returning `success: false`). Terminal.
    #[error("Linear API error: {0}")]
    Api(String),

    /// Transport-level failure (connect/timeout/TLS). Retried.
    #[error("Linear request failed: {0}")]
    Http(String),

    /// The HTTP call succeeded but the body was not the expected JSON shape.
    /// Terminal — retrying an unparseable response won't help.
    #[error("Linear response malformed: {0}")]
    Malformed(String),

    /// No configured account matches the requested key. Terminal (config bug).
    #[error("unknown Linear account key: {0}")]
    UnknownAccount(String),
}

impl LinearError {
    /// Whether a `backon` retry is worthwhile. Only transient transport and
    /// rate-limit failures qualify; auth/not-found/malformed/user errors are
    /// deterministic and would fail identically on retry.
    pub fn should_retry(&self) -> bool {
        matches!(self, LinearError::RateLimited | LinearError::Http(_))
    }
}
