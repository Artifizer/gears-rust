//! REST error mapping: `DomainError` -> `CanonicalError` (AIP-193 / RFC-9457
//! wire format), per ADR-0005 (`toolkit-canonical-errors`).
//!
//! The mapping goes from `DomainError` directly rather than through the SDK's
//! `PromptScannerError` (TOOLKIT-ERR-002's usual route): the SDK error folds
//! a validation error's field into its message and database errors into
//! `Internal`, so the field violation and the log context would be lost, and
//! the orphan rule forbids `From<PromptScannerError> for CanonicalError` here
//! anyway. Both mappings are exhaustive over the same variants and share the
//! rule that no free-text reason reaches the wire.
//!
//! Status codes: not found 404, validation and attachment problems 400,
//! authorization 403, a read-only policy provider 501, a timeout 504, an
//! unavailable dependency or a full queue 503, anything else 500.

use toolkit::api::canonical_prelude::*;

use crate::domain::error::DomainError;

#[resource_error(gts_id!("cf.core.prompt_scanner.scan_record.v1~"))]
struct ScanRecordResourceError;

#[resource_error(gts_id!("cf.core.prompt_scanner.tenant_policy.v1~"))]
struct TenantPolicyResourceError;

/// Seconds a client should wait before retrying a 503.
const RETRY_AFTER_SECONDS: u64 = 1;

fn unavailable(detail: &str) -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail(detail)
        .with_retry_after_seconds(RETRY_AFTER_SECONDS)
        .create()
}

impl From<DomainError> for CanonicalError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::VerdictNotFound { trace_id } => ScanRecordResourceError::not_found(
                format!("scan verdict with trace_id {trace_id} was not found"),
            )
            .with_resource(trace_id.to_string())
            .create(),

            DomainError::JobNotFound { job_id } => ScanRecordResourceError::not_found(format!(
                "scan job with job_id {job_id} was not found"
            ))
            .with_resource(job_id.to_string())
            .create(),

            DomainError::Validation { field, message } => {
                ScanRecordResourceError::invalid_argument()
                    .with_field_violation(field, message, "VALIDATION")
                    .create()
            }

            DomainError::Forbidden => ScanRecordResourceError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),

            // Logged by the service where it happened.
            DomainError::BackendTimeout { .. } => ScanRecordResourceError::deadline_exceeded(
                "a scanner backend did not respond in time",
            )
            .create(),

            DomainError::BackendUnavailable { .. } | DomainError::RequiredBackendFailed { .. } => {
                unavailable("a scanner backend the tenant policy requires is unavailable")
            }

            DomainError::PolicyUnavailable { tenant_id, .. } => unavailable(&format!(
                "policy for tenant {tenant_id} is temporarily unavailable"
            )),

            DomainError::Overloaded => unavailable("prompt-scanner is at capacity; retry later"),

            DomainError::AttachmentParsingFailed { kind, .. } => {
                ScanRecordResourceError::invalid_argument()
                    .with_field_violation(
                        "attachment",
                        "the attachment could not be turned into text",
                        kind.code(),
                    )
                    .create()
            }

            DomainError::Unsupported { operation } => TenantPolicyResourceError::unimplemented(
                format!("{operation} is not supported by the active policy provider"),
            )
            .create(),

            DomainError::Database { .. } => {
                tracing::error!(error = ?e, "database error");
                CanonicalError::internal("An internal database error occurred").create()
            }

            DomainError::Internal(_) => {
                tracing::error!(error = ?e, "internal error");
                CanonicalError::internal("An internal error occurred").create()
            }
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
