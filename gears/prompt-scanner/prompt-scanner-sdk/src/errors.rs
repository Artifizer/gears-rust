//! Transport-agnostic error type for the prompt-scanner gear.

use thiserror::Error;
use uuid::Uuid;

use crate::models::ScanJobFailureCode;

/// Errors that can occur when using the prompt-scanner API.
///
/// Free-text `reason`/`message` fields are for logs: they may carry upstream
/// error text and must not be put on the wire as-is.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum PromptScannerError {
    /// No verdict recorded for the given trace id.
    #[error("scan verdict not found: {trace_id}")]
    VerdictNotFound {
        /// The trace id that was not found.
        trace_id: Uuid,
    },

    /// No async job with this id is known for the caller's tenant (never
    /// queued, already forgotten, or another tenant's).
    #[error("scan job not found: {job_id}")]
    JobNotFound {
        /// The job id that was not found.
        job_id: Uuid,
    },

    /// The request failed validation.
    #[error("validation error: {message}")]
    Validation {
        /// Human-readable description of the validation failure.
        message: String,
    },

    /// The caller is not authorized to perform the requested operation.
    #[error("access denied")]
    Forbidden,

    /// A scanner backend did not respond within the configured timeout.
    #[error("backend '{backend}' timed out")]
    BackendTimeout {
        /// Vendor identifier of the backend.
        backend: String,
    },

    /// A scanner backend is not currently resolvable/available.
    #[error("backend '{backend}' unavailable: {reason}")]
    BackendUnavailable {
        /// Vendor identifier of the backend.
        backend: String,
        /// Human-readable reason.
        reason: String,
    },

    /// A required backend (per tenant policy) failed or timed out.
    #[error("required backend '{backend}' failed: {reason}")]
    RequiredBackendFailed {
        /// Vendor identifier of the required backend.
        backend: String,
        /// Human-readable reason.
        reason: String,
    },

    /// The tenant's policy could not be loaded: its policy provider's source
    /// is unreachable and the provider has nothing to serve in its place.
    /// The caller is not at fault; retrying later may succeed.
    #[error("policy for tenant {tenant_id} unavailable: {reason}")]
    PolicyUnavailable {
        /// The tenant whose policy could not be loaded.
        tenant_id: Uuid,
        /// Human-readable reason, for logs only: it may name hosts or carry
        /// upstream error text, so it never reaches the wire.
        reason: String,
    },

    /// Extracting text from a `ContentKind::Upload` attachment failed (e.g.
    /// unsupported file type, corrupt file, or `file-parser` itself was
    /// unreachable).
    #[error("attachment parsing failed: {reason}")]
    AttachmentParsingFailed {
        /// Human-readable reason.
        reason: String,
    },

    /// The operation is not supported by this deployment, e.g. writing a
    /// policy when the active policy provider is read-only.
    #[error("operation not supported: {operation}")]
    Unsupported {
        /// The operation that was refused (e.g. `"put_policy"`).
        operation: String,
    },

    /// The gateway is at capacity (e.g. the async job queue is full).
    /// Retrying later may succeed.
    #[error("prompt-scanner is overloaded")]
    Overloaded,

    /// An unexpected internal error occurred.
    #[error("internal error: {0}")]
    Internal(String),
}

impl PromptScannerError {
    /// Construct a [`PromptScannerError::VerdictNotFound`].
    #[must_use]
    pub fn verdict_not_found(trace_id: Uuid) -> Self {
        Self::VerdictNotFound { trace_id }
    }

    /// Construct a [`PromptScannerError::JobNotFound`].
    #[must_use]
    pub fn job_not_found(job_id: Uuid) -> Self {
        Self::JobNotFound { job_id }
    }

    /// Construct a [`PromptScannerError::BackendTimeout`].
    pub fn backend_timeout(backend: impl Into<String>) -> Self {
        Self::BackendTimeout {
            backend: backend.into(),
        }
    }

    /// Construct a [`PromptScannerError::Unsupported`].
    pub fn unsupported(operation: impl Into<String>) -> Self {
        Self::Unsupported {
            operation: operation.into(),
        }
    }

    /// Construct a [`PromptScannerError::PolicyUnavailable`].
    pub fn policy_unavailable(tenant_id: Uuid, reason: impl Into<String>) -> Self {
        Self::PolicyUnavailable {
            tenant_id,
            reason: reason.into(),
        }
    }

    /// Construct a [`PromptScannerError::Validation`].
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    /// Construct a [`PromptScannerError::Internal`].
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    /// The stable code reported in [`ScanJobStatus::Failed`](crate::ScanJobStatus::Failed) when an async
    /// scan job fails with this error.
    #[must_use]
    pub fn job_failure_code(&self) -> ScanJobFailureCode {
        match self {
            Self::Validation { .. } => ScanJobFailureCode::Validation,
            Self::Forbidden => ScanJobFailureCode::Forbidden,
            Self::PolicyUnavailable { .. } => ScanJobFailureCode::PolicyUnavailable,
            Self::BackendTimeout { .. }
            | Self::BackendUnavailable { .. }
            | Self::RequiredBackendFailed { .. } => ScanJobFailureCode::RequiredBackendFailed,
            Self::AttachmentParsingFailed { .. } => ScanJobFailureCode::AttachmentParsingFailed,
            Self::Overloaded => ScanJobFailureCode::Overloaded,
            Self::VerdictNotFound { .. }
            | Self::JobNotFound { .. }
            | Self::Unsupported { .. }
            | Self::Internal(_) => ScanJobFailureCode::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_failure_codes_hide_details() {
        assert_eq!(
            PromptScannerError::backend_timeout("pii").job_failure_code(),
            ScanJobFailureCode::RequiredBackendFailed
        );
        assert_eq!(
            PromptScannerError::internal("db host 10.0.0.1 down").job_failure_code(),
            ScanJobFailureCode::Internal
        );
        assert_eq!(
            PromptScannerError::Overloaded.job_failure_code(),
            ScanJobFailureCode::Overloaded
        );
    }
}
