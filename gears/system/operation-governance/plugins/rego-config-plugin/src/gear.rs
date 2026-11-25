//! Rego config plugin gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use operation_governance_sdk::{
    OperationGovernancePluginClientV1, OperationGovernancePluginSpecV1,
};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::OgRegoConfigPluginConfig;
use crate::domain::Service;

/// GTS instance suffix identifying this backend.
pub const INSTANCE_SUFFIX: &str = "cf.builtin.og_rego_config.plugin.v1";

/// Operation Governance plugin that loads Rego policies from configuration.
#[toolkit::gear(
    name = "og-rego-config-plugin",
    deps = [types_registry]
)]
pub struct OgRegoConfigPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for OgRegoConfigPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for OgRegoConfigPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: OgRegoConfigPluginConfig = ctx.config_or_default()?;

        // Compile before registering: a bad policy must abort startup rather
        // than advertise a backend that cannot decide.
        let service = Arc::new(Service::from_config(&cfg)?);
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            operations = service.operation_count(),
            "Loaded governance policies"
        );

        let (instance_id, instance_json) =
            PluginV1::<OperationGovernancePluginSpecV1>::build_registration(
                INSTANCE_SUFFIX,
                cfg.vendor.clone(),
                cfg.priority,
            )?;
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let api: Arc<dyn OperationGovernancePluginClientV1> = service;
        ctx.client_hub()
            .register_scoped::<dyn OperationGovernancePluginClientV1>(
                ClientScope::gts_id(&instance_id),
                api,
            );
        info!(instance_id = %instance_id, "Registered governance plugin");
        Ok(())
    }
}
