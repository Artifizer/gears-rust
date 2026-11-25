use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

#[resource_error(gts_id!("x.object_storage.storage.object.v1~"))]
pub struct ObjectError;

impl From<DomainError> for CanonicalError {
    #[allow(clippy::cognitive_complexity)]
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::NotFound(id) => ObjectError::not_found("Object not found")
                .with_resource(id.to_string())
                .create(),
            DomainError::Validation { field, message } => ObjectError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION_ERROR")
                .create(),
            DomainError::Forbidden(msg) => {
                tracing::warn!(msg = %msg, "object-storage access forbidden");
                // Mask as not found: do not disclose that an object exists.
                ObjectError::not_found("Object not found or not accessible")
                    .with_resource("object")
                    .create()
            }
            // Governance refusals carry the message the policy author wrote.
            DomainError::GovernanceBlocked(findings) => ObjectError::permission_denied()
                .with_reason(format!("GOVERNANCE_BLOCKED: {findings}"))
                .create(),
            DomainError::ApprovalRequired(findings) => ObjectError::failed_precondition()
                .with_precondition_violation("approval", findings, "APPROVAL_REQUIRED")
                .create(),
            DomainError::GovernanceUnavailable { message, retryable } => {
                tracing::error!(msg = %message, "object-storage governance unavailable");
                let builder = CanonicalError::service_unavailable()
                    .with_detail("Operation governance could not decide");
                if retryable {
                    builder.with_retry_after_seconds(1).create()
                } else {
                    builder.create()
                }
            }
            DomainError::Internal(msg) => {
                tracing::error!(msg = %msg, "object-storage internal error");
                CanonicalError::internal(msg).create()
            }
            DomainError::Database(db_err) => {
                tracing::error!(error = ?db_err, "object-storage database error");
                CanonicalError::internal(db_err.to_string()).create()
            }
        }
    }
}
