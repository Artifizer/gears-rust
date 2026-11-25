use toolkit_db::DbError;
use toolkit_macros::domain_model;
use uuid::Uuid;

#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("object {0} not found")]
    NotFound(Uuid),

    #[error("validation error on field '{field}': {message}")]
    Validation { field: String, message: String },

    #[error("access forbidden: {0}")]
    Forbidden(String),

    /// A guardrail policy blocks the operation.
    #[error("blocked by policy: {0}")]
    GovernanceBlocked(String),

    /// The operation needs approval before it may run.
    #[error("approval required: {0}")]
    ApprovalRequired(String),

    /// Governance could not decide; the operation does not run.
    #[error("governance unavailable: {message}")]
    GovernanceUnavailable { message: String, retryable: bool },

    #[error("internal error: {0}")]
    Internal(String),

    #[error("database error: {0}")]
    Database(#[from] DbError),
}

impl DomainError {
    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation {
            field: field.into(),
            message: message.into(),
        }
    }
}

// The enforcer error carries no source worth keeping beyond its message here.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        match e {
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden(e.to_string()),
            authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => Self::Internal(e.to_string()),
        }
    }
}
