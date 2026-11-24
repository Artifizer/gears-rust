//! Local (in-process) client for the prompt-scanner gear.

use std::sync::Arc;

use async_trait::async_trait;
use prompt_scanner_sdk::{
    PromptScannerClientV1, PromptScannerError, ScanJobHandle, ScanJobStatus, ScanRequest,
    ScanVerdict, TenantPolicy,
};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::repos::EvidenceRepository;
use super::service::Service;

/// Local client wrapping the prompt-scanner domain service.
///
/// Registered in `ClientHub` by the gear during `init()`.
#[domain_model]
pub struct PromptScannerLocalClient<SR>
where
    SR: EvidenceRepository + 'static,
{
    svc: Arc<Service<SR>>,
}

impl<SR> PromptScannerLocalClient<SR>
where
    SR: EvidenceRepository + 'static,
{
    #[must_use]
    pub fn new(svc: Arc<Service<SR>>) -> Self {
        Self { svc }
    }
}

/// Convert for the caller. Database and internal errors are logged here,
/// once, with their source; the service has already logged backend, policy
/// and authorization failures with their context, and a caller's own fault
/// is not an error of ours.
fn log_and_convert(op: &str, e: super::DomainError) -> PromptScannerError {
    match &e {
        super::DomainError::Database { .. } | super::DomainError::Internal(_) => {
            tracing::error!(operation = op, error = ?e, "prompt-scanner call failed");
        }
        other if other.is_caller_fault() => {
            tracing::debug!(operation = op, error = %other, "prompt-scanner call refused");
        }
        _ => {}
    }
    e.into()
}

#[async_trait]
impl<SR> PromptScannerClientV1 for PromptScannerLocalClient<SR>
where
    SR: EvidenceRepository + 'static,
{
    async fn scan(
        &self,
        ctx: &SecurityContext,
        req: ScanRequest,
    ) -> Result<ScanVerdict, PromptScannerError> {
        self.svc
            .scan(ctx, req)
            .await
            .map_err(|e| log_and_convert("scan", e))
    }

    async fn scan_async(
        &self,
        ctx: &SecurityContext,
        req: ScanRequest,
    ) -> Result<ScanJobHandle, PromptScannerError> {
        self.svc
            .scan_async(ctx, req)
            .await
            .map_err(|e| log_and_convert("scan_async", e))
    }

    async fn get_job_status(
        &self,
        ctx: &SecurityContext,
        job_id: Uuid,
    ) -> Result<ScanJobStatus, PromptScannerError> {
        self.svc
            .get_job_status(ctx, job_id)
            .await
            .map_err(|e| log_and_convert("get_job_status", e))
    }

    async fn get_verdict(
        &self,
        ctx: &SecurityContext,
        trace_id: Uuid,
    ) -> Result<ScanVerdict, PromptScannerError> {
        self.svc
            .get_verdict(ctx, trace_id)
            .await
            .map_err(|e| log_and_convert("get_verdict", e))
    }

    async fn find_by_correlation_id(
        &self,
        ctx: &SecurityContext,
        correlation_id: &str,
    ) -> Result<Vec<ScanVerdict>, PromptScannerError> {
        self.svc
            .find_by_correlation_id(ctx, correlation_id)
            .await
            .map_err(|e| log_and_convert("find_by_correlation_id", e))
    }

    async fn delete_evidence(
        &self,
        ctx: &SecurityContext,
        trace_id: Uuid,
    ) -> Result<u64, PromptScannerError> {
        self.svc
            .delete_evidence(ctx, trace_id)
            .await
            .map_err(|e| log_and_convert("delete_evidence", e))
    }

    async fn get_policy(&self, ctx: &SecurityContext) -> Result<TenantPolicy, PromptScannerError> {
        self.svc
            .get_policy(ctx)
            .await
            .map_err(|e| log_and_convert("get_policy", e))
    }

    async fn put_policy(
        &self,
        ctx: &SecurityContext,
        policy: TenantPolicy,
    ) -> Result<TenantPolicy, PromptScannerError> {
        self.svc
            .put_policy(ctx, policy)
            .await
            .map_err(|e| log_and_convert("put_policy", e))
    }
}
