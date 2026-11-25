use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::Router;
use object_storage_sdk::ObjectStorageClientV1;
use operation_governance_sdk::{EnforcerConfig, GovernanceEnforcer};
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx};
use toolkit_db::{DBProvider, DbError};
use tracing::info;

use crate::api::rest::routes::{self, ConcreteService};
use crate::config::ObjectStorageConfig;
use crate::domain::local_client::LocalClient;
use crate::domain::service::{Service, ServiceConfig};
use crate::infra::storage::sea_orm_repo::SeaOrmObjectsRepository;

#[toolkit::gear(
    name = "object-storage",
    deps = [authz_resolver, types_registry],
    capabilities = [rest, db]
)]
pub struct ObjectStorageGear {
    service: OnceLock<Arc<ConcreteService>>,
}

impl Default for ObjectStorageGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

impl toolkit::contracts::DatabaseCapability for ObjectStorageGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

#[async_trait]
impl Gear for ObjectStorageGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: ObjectStorageConfig = ctx.config_or_default()?;
        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;

        // The Governance Enforcer finds the governance plugin (for example the
        // Rego config plugin) through types-registry on the first check, by
        // vendor and priority, like a gateway gear finds its plugins.
        let governance = Arc::new(GovernanceEnforcer::new(
            ctx.client_hub(),
            EnforcerConfig {
                vendor: cfg.governance_vendor.clone(),
                max_applicability_lease: Duration::from_millis(cfg.governance_max_lease_ms),
            },
        ));

        let service = Arc::new(Service::new(
            db,
            Arc::new(SeaOrmObjectsRepository::new()),
            PolicyEnforcer::new(authz),
            governance,
            ServiceConfig {
                max_body_bytes: cfg.max_body_bytes,
                ..ServiceConfig::default()
            },
        ));
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let client: Arc<dyn ObjectStorageClientV1> = Arc::new(LocalClient::new(service));
        ctx.client_hub().register(client);
        info!("object-storage initialized");
        Ok(())
    }
}

impl toolkit::contracts::RestApiCapability for ObjectStorageGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();
        Ok(routes::register_routes(router, openapi, service))
    }
}
