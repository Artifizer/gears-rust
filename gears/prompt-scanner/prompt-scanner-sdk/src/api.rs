//! Public API trait for the prompt-scanner gear.
//!
//! Connectors (`llm-gateway`'s `scan-hook-plugin`, endpoint agents, MCP
//! proxies, ...) call this trait to submit content for scanning and to
//! manage per-tenant backend policy.

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::PromptScannerError;
use crate::models::{ScanJobHandle, ScanJobStatus, ScanRequest, ScanVerdict, TenantPolicy};

/// Public API trait for the prompt-scanner gear.
///
/// Obtain via `ClientHub`:
/// ```ignore
/// let client = hub.get::<dyn PromptScannerClientV1>()?;
/// let verdict = client.scan(&ctx, req).await?;
/// ```
#[async_trait]
pub trait PromptScannerClientV1: Send + Sync {
    /// Synchronously scan a piece of content and return the aggregated
    /// verdict. This is the main connector ingestion path.
    ///
    /// # Errors
    ///
    /// The returned verdict carries `redacted_payload` when its action is
    /// `Redact`.
    ///
    /// # Errors
    ///
    /// - `Validation` if the request is malformed (see
    ///   [`ScanRequest::validate`]).
    /// - `Forbidden` if the caller may not scan for its tenant.
    /// - `RequiredBackendFailed` / `BackendUnavailable` / `BackendTimeout` if
    ///   a tenant-required backend errored, was missing or timed out.
    /// - `PolicyUnavailable` if the tenant's policy could not be loaded.
    /// - `AttachmentParsingFailed` if text could not be extracted from a
    ///   [`ScanContent::File`](crate::ScanContent::File).
    async fn scan(
        &self,
        ctx: &SecurityContext,
        req: ScanRequest,
    ) -> Result<ScanVerdict, PromptScannerError>;

    /// Queue a scan for asynchronous processing (e.g. large uploads) and
    /// return a job handle. Poll [`Self::get_job_status`] with the returned
    /// `job_id`; once it is `Completed`, a flagged scan's findings are
    /// available via [`Self::get_verdict`] with `trace_id`. The verdict of an
    /// async scan never carries `redacted_payload`.
    ///
    /// # Errors
    ///
    /// - `Validation` if the request is malformed.
    /// - `Forbidden` if the caller may not scan for its tenant.
    /// - `Overloaded` if the job queue is full.
    async fn scan_async(
        &self,
        ctx: &SecurityContext,
        req: ScanRequest,
    ) -> Result<ScanJobHandle, PromptScannerError>;

    /// Current status of a job queued by [`Self::scan_async`] for the
    /// caller's tenant. A failed job reports a stable
    /// [`ScanJobFailureCode`](crate::ScanJobFailureCode), never the error text.
    ///
    /// # Errors
    ///
    /// - `JobNotFound` if the job is unknown, already forgotten, or belongs
    ///   to another tenant. These cases are not told apart.
    async fn get_job_status(
        &self,
        ctx: &SecurityContext,
        job_id: Uuid,
    ) -> Result<ScanJobStatus, PromptScannerError>;

    /// Fetch the recorded findings of a scan by trace id (traceability /
    /// audit lookups).
    ///
    /// Only a scan with findings is recorded, and only until its retention
    /// expires. The returned verdict carries the stored findings and the
    /// scan's verdict action; it never carries the payload.
    ///
    /// # Errors
    ///
    /// - `VerdictNotFound` if the scan had no findings, is unknown, has
    ///   expired, or belongs to another tenant. These cases are not told apart.
    async fn get_verdict(
        &self,
        ctx: &SecurityContext,
        trace_id: Uuid,
    ) -> Result<ScanVerdict, PromptScannerError>;

    /// Every recorded scan of the caller's tenant whose request carried
    /// `correlation_id` (correlation lookup). Newest first, at most 1,000
    /// findings across the returned scans. Empty when nothing matches.
    ///
    /// # Errors
    ///
    /// Returns [`PromptScannerError`] on authorization failure.
    async fn find_by_correlation_id(
        &self,
        ctx: &SecurityContext,
        correlation_id: &str,
    ) -> Result<Vec<ScanVerdict>, PromptScannerError>;

    /// Remove a scan's recorded findings and evidence before they expire
    /// (manual evidence removal). A hard delete. Returns the number of
    /// finding rows removed.
    ///
    /// # Errors
    ///
    /// - `VerdictNotFound` if nothing was recorded for `trace_id` in the
    ///   caller's tenant.
    async fn delete_evidence(
        &self,
        ctx: &SecurityContext,
        trace_id: Uuid,
    ) -> Result<u64, PromptScannerError>;

    /// Get the calling tenant's backend policy. When nothing is configured
    /// for the tenant this returns the deployment's default policy (by
    /// default [`TenantPolicy::default_for_tenant`]), never an error.
    ///
    /// # Errors
    ///
    /// - `Forbidden` if the caller may not read its tenant's policy.
    /// - `PolicyUnavailable` if the policy source is unreachable.
    async fn get_policy(&self, ctx: &SecurityContext) -> Result<TenantPolicy, PromptScannerError>;

    /// Update the calling tenant's backend policy.
    ///
    /// # Errors
    ///
    /// - `Validation` if the policy references unknown backends or invalid
    ///   thresholds.
    /// - `Forbidden` if the caller may not update its tenant's policy.
    /// - `Unsupported` if the active policy provider is read-only.
    async fn put_policy(
        &self,
        ctx: &SecurityContext,
        policy: TenantPolicy,
    ) -> Result<TenantPolicy, PromptScannerError>;
}
