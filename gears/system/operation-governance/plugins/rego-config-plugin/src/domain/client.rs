//! The plugin contract, served from the compiled policies.

use async_trait::async_trait;
use operation_governance_sdk::{
    Applicability, CheckResponse, GovernanceCheckRequest, OgError,
    OperationGovernancePluginClientV1,
};
use toolkit_security::SecurityContext;

use super::service::Service;

#[async_trait]
impl OperationGovernancePluginClientV1 for Service {
    async fn applicability(
        &self,
        _ctx: &SecurityContext,
        operation: &str,
    ) -> Result<Applicability, OgError> {
        // Policies are global in this plugin, so applicability does not depend
        // on the caller's tenant.
        Ok(Service::applicability(self, operation))
    }

    async fn check(
        &self,
        ctx: &SecurityContext,
        request: &GovernanceCheckRequest,
    ) -> Result<CheckResponse, OgError> {
        Service::check(self, ctx, request)
    }
}
