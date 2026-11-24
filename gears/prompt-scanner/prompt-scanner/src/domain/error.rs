//! Domain error type for the prompt-scanner gear.
//!
//! Free-text `reason`/`message` fields are for logs: the REST mapping
//! (`api::rest::error`) never puts them on the wire, except a validation
//! message, which describes the caller's own input.

use std::sync::Arc;

use prompt_scanner_sdk::PromptScannerError;
use thiserror::Error;
use toolkit_db::DbError;
use toolkit_db::secure::ScopeError;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// A database failure, with its source kept for the logs.
pub type DbSource = Arc<dyn std::error::Error + Send + Sync + 'static>;

/// Why an attachment could not be turned into text. Stable, safe to show to
/// the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentFailure {
    /// No parser handles this file type.
    UnsupportedType,
    /// The file was refused before parsing (too big, empty, ...).
    Rejected,
    /// The file could not be parsed, or held no text.
    Unparseable,
}

impl AttachmentFailure {
    /// The reason code reported to the caller.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::UnsupportedType => "ATTACHMENT_UNSUPPORTED_TYPE",
            Self::Rejected => "ATTACHMENT_REJECTED",
            Self::Unparseable => "ATTACHMENT_PARSE_FAILED",
        }
    }
}

/// Domain-specific errors.
#[domain_model]
#[derive(Error, Debug, Clone)]
pub enum DomainError {
    #[error("scan verdict not found: {trace_id}")]
    VerdictNotFound { trace_id: Uuid },

    #[error("scan job not found: {job_id}")]
    JobNotFound { job_id: Uuid },

    #[error("validation error: {field}: {message}")]
    Validation { field: String, message: String },

    #[error("access denied")]
    Forbidden,

    #[error("backend '{backend}' timed out")]
    BackendTimeout { backend: String },

    #[error("backend '{backend}' unavailable: {reason}")]
    BackendUnavailable { backend: String, reason: String },

    #[error("required backend '{backend}' failed: {reason}")]
    RequiredBackendFailed { backend: String, reason: String },

    #[error("policy for tenant {tenant_id} unavailable: {reason}")]
    PolicyUnavailable { tenant_id: Uuid, reason: String },

    #[error("attachment parsing failed: {reason}")]
    AttachmentParsingFailed {
        kind: AttachmentFailure,
        /// For logs only.
        reason: String,
    },

    #[error("operation not supported: {operation}")]
    Unsupported { operation: String },

    #[error("prompt-scanner is overloaded")]
    Overloaded,

    // The cause is part of the message so logs (`error = %e`) show it; the
    // REST layer never puts this text on the wire (fixed 500 message).
    #[error("database error: {context}: {source}")]
    Database {
        context: &'static str,
        #[source]
        source: DbSource,
    },

    #[error("internal error: {0}")]
    Internal(String),
}

/// A row read back that the domain model cannot represent.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct MappingError(pub String);

impl DomainError {
    #[must_use]
    pub fn verdict_not_found(trace_id: Uuid) -> Self {
        Self::VerdictNotFound { trace_id }
    }

    #[must_use]
    pub fn job_not_found(job_id: Uuid) -> Self {
        Self::JobNotFound { job_id }
    }

    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation {
            field: field.into(),
            message: message.into(),
        }
    }

    pub fn unsupported(operation: impl Into<String>) -> Self {
        Self::Unsupported {
            operation: operation.into(),
        }
    }

    /// A database failure in `context`, keeping `source` for the logs.
    pub fn database(
        context: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Database {
            context,
            source: Arc::new(source),
        }
    }

    /// A row the domain model cannot represent.
    pub fn mapping(message: impl Into<String>) -> Self {
        Self::database("mapping a stored row", MappingError(message.into()))
    }

    /// Whether the caller is at fault (a 4xx), as opposed to the gateway or
    /// a dependency.
    #[must_use]
    pub fn is_caller_fault(&self) -> bool {
        matches!(
            self,
            Self::VerdictNotFound { .. }
                | Self::JobNotFound { .. }
                | Self::Validation { .. }
                | Self::Forbidden
                | Self::Unsupported { .. }
                | Self::AttachmentParsingFailed { .. }
        )
    }
}

impl From<DbError> for DomainError {
    fn from(e: DbError) -> Self {
        Self::database("database", e)
    }
}

/// A scope refusal is an authorization outcome (403), not a database fault.
impl From<ScopeError> for DomainError {
    fn from(e: ScopeError) -> Self {
        match e {
            ScopeError::Denied(_) | ScopeError::TenantNotInScope { .. } => Self::Forbidden,
            other => Self::database("secure query", other),
        }
    }
}

/// Logging is the caller's job (`Service::authorize`), where the resource and
/// action are known.
impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        match e {
            authz_resolver_sdk::EnforcerError::AccessDenied(_)
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden,
            // `EnforcerError` is non-exhaustive: an outcome class added later is
            // not a grant, so it fails closed as an internal error.
            _ => Self::Internal("authz evaluation failed".to_owned()),
        }
    }
}

// `Internal` holds text for the logs only; it never reaches the wire.
#[allow(
    unknown_lints,
    de1302_error_from_to_string,
    reason = "dylint-only lint: `#[expect]` would be unfulfilled under plain clippy"
)]
impl From<toolkit_canonical_errors::CanonicalError> for DomainError {
    fn from(e: toolkit_canonical_errors::CanonicalError) -> Self {
        Self::Internal(e.diagnostic().map_or_else(|| e.to_string(), str::to_owned))
    }
}

#[allow(
    unknown_lints,
    de1302_error_from_to_string,
    reason = "dylint-only lint: `#[expect]` would be unfulfilled under plain clippy"
)]
impl From<toolkit::client_hub::ClientHubError> for DomainError {
    fn from(e: toolkit::client_hub::ClientHubError) -> Self {
        Self::Internal(e.to_string())
    }
}

/// A file-parser outage is ours (500); a file it cannot handle is the
/// caller's (400).
impl From<file_parser_sdk::FileParserClientError> for DomainError {
    fn from(e: file_parser_sdk::FileParserClientError) -> Self {
        use file_parser_sdk::FileParserClientError as E;
        let kind = match &e {
            E::Internal(message) => {
                return Self::Internal(format!("file-parser: {message}"));
            }
            E::UnsupportedFileType { .. } => AttachmentFailure::UnsupportedType,
            E::InvalidRequest { .. } => AttachmentFailure::Rejected,
            E::ParseFailed { .. } => AttachmentFailure::Unparseable,
        };
        Self::AttachmentParsingFailed {
            kind,
            reason: e.to_string(),
        }
    }
}

/// Domain → SDK error conversion.
impl From<DomainError> for PromptScannerError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::VerdictNotFound { trace_id } => Self::verdict_not_found(trace_id),
            DomainError::JobNotFound { job_id } => Self::job_not_found(job_id),
            DomainError::Validation { field, message } => {
                Self::validation(format!("{field}: {message}"))
            }
            DomainError::Forbidden => Self::Forbidden,
            DomainError::BackendTimeout { backend } => Self::BackendTimeout { backend },
            DomainError::BackendUnavailable { backend, reason } => {
                Self::BackendUnavailable { backend, reason }
            }
            DomainError::RequiredBackendFailed { backend, reason } => {
                Self::RequiredBackendFailed { backend, reason }
            }
            DomainError::PolicyUnavailable { tenant_id, reason } => {
                Self::PolicyUnavailable { tenant_id, reason }
            }
            DomainError::AttachmentParsingFailed { kind, .. } => Self::AttachmentParsingFailed {
                reason: kind.code().to_owned(),
            },
            DomainError::Unsupported { operation } => Self::Unsupported { operation },
            DomainError::Overloaded => Self::Overloaded,
            DomainError::Database { context, .. } => {
                Self::internal(format!("database error: {context}"))
            }
            DomainError::Internal(message) => Self::internal(message),
        }
    }
}

/// SDK → domain error conversion — the reverse of the `From<DomainError> for
/// PromptScannerError` impl above. Needed because `Service` calls through the
/// transport-agnostic `PolicyProvider` trait, which returns
/// `PromptScannerError`.
#[allow(
    unknown_lints,
    de1302_error_from_to_string,
    reason = "dylint-only lint: `#[expect]` would be unfulfilled under plain clippy"
)]
impl From<PromptScannerError> for DomainError {
    fn from(e: PromptScannerError) -> Self {
        match e {
            PromptScannerError::VerdictNotFound { trace_id } => Self::VerdictNotFound { trace_id },
            PromptScannerError::JobNotFound { job_id } => Self::JobNotFound { job_id },
            PromptScannerError::Validation { message } => Self::Validation {
                field: "policy".to_owned(),
                message,
            },
            PromptScannerError::Forbidden => Self::Forbidden,
            PromptScannerError::BackendTimeout { backend } => Self::BackendTimeout { backend },
            PromptScannerError::BackendUnavailable { backend, reason } => {
                Self::BackendUnavailable { backend, reason }
            }
            PromptScannerError::RequiredBackendFailed { backend, reason } => {
                Self::RequiredBackendFailed { backend, reason }
            }
            PromptScannerError::PolicyUnavailable { tenant_id, reason } => {
                Self::PolicyUnavailable { tenant_id, reason }
            }
            PromptScannerError::AttachmentParsingFailed { reason } => {
                Self::AttachmentParsingFailed {
                    kind: AttachmentFailure::Unparseable,
                    reason,
                }
            }
            PromptScannerError::Unsupported { operation } => Self::Unsupported { operation },
            PromptScannerError::Overloaded => Self::Overloaded,
            PromptScannerError::Internal(message) => Self::Internal(message),
            // `PromptScannerError` is non-exhaustive.
            other => Self::Internal(other.to_string()),
        }
    }
}
