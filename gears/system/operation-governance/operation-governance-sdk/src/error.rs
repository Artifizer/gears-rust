//! Errors of the governance plugin contract.

/// Error a governance backend returns when it cannot answer.
///
/// A decided refusal is never an error: it is a
/// [`GovernanceOutcome`](crate::GovernanceOutcome).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OgError {
    /// The request is malformed for the operation (an unknown operation type,
    /// an undeclared Fact, a Fact that does not match its schema).
    #[error("invalid governance request: {0}")]
    Invalid(String),
    /// The backend cannot decide now; a retry may help.
    #[error("governance backend unavailable: {0}")]
    Unavailable(String),
    /// A defect in the backend or the contract; a retry will not help.
    #[error("governance backend error: {0}")]
    Internal(String),
}
