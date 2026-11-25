//! The contract a governance backend implements.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::error::OgError;
use crate::models::{Applicability, CheckResponse, GovernanceCheckRequest};

/// A governance backend: decides Governance Checks for Governed Operations.
///
/// Registered in `ClientHub` scoped by the backend's plugin instance ID (an
/// instance of [`OperationGovernancePluginSpecV1`](crate::OperationGovernancePluginSpecV1)).
/// The [`GovernanceEnforcer`](crate::GovernanceEnforcer) selects one instance
/// by vendor and priority.
#[async_trait]
pub trait OperationGovernancePluginClientV1: Send + Sync {
    /// Whether any policy governs `operation` for the caller's tenant, and
    /// which Facts the applicable policies read. Must not evaluate anything:
    /// the Enforcer calls it to decide whether a check is needed at all.
    async fn applicability(
        &self,
        ctx: &SecurityContext,
        operation: &str,
    ) -> Result<Applicability, OgError>;

    /// Decide one check, or ask for Facts the request did not carry.
    async fn check(
        &self,
        ctx: &SecurityContext,
        request: &GovernanceCheckRequest,
    ) -> Result<CheckResponse, OgError>;
}
