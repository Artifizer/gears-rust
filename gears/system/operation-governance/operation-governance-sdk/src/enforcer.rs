//! The Governance Enforcer: the enforcement point inside a Consuming Gear.
//!
//! The gear's domain service calls [`GovernanceEnforcer::check`] after
//! Authorization (and after an idempotent replay of a completed operation,
//! which is never checked again), and before the first side effect:
//!
//! 1. Look up applicability for the operation and tenant (cached). If no
//!    policy governs it, return Allowed: no Fact is loaded, no check is made.
//! 2. Ask the gear's [`FactProvider`] for only the Facts the applicable
//!    policies read, one call per Fact Tier, cheapest first.
//! 3. Make one Governance Check. If the backend answers with missing Facts
//!    (its policies changed since the cached applicability), load them and
//!    check once more. A second miss is Unavailable.
//!
//! Every failure is a refusal: the Enforcer never returns Allowed because
//! something could not be checked.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{GtsPluginSelector, choose_plugin_instance};
use toolkit_security::SecurityContext;
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};
use uuid::Uuid;

use crate::error::OgError;
use crate::facts::{FactError, FactProvider, FactSchema, FactWriter};
use crate::gts::OperationGovernancePluginSpecV1;
use crate::models::{
    Applicability, CheckMode, CheckResponse, FactSet, GovernanceCheckRequest, GovernanceOutcome,
    GovernedResource, operation_family,
};
use crate::plugin_api::OperationGovernancePluginClientV1;

/// Settings of the Enforcer.
#[derive(Debug, Clone)]
pub struct EnforcerConfig {
    /// Vendor of the governance plugin instance to use.
    pub vendor: String,
    /// Upper bound on the applicability lease the backend grants. The
    /// Enforcer caches an answer for the shorter of the two, so a backend can
    /// never make a gear keep stale rules longer than this.
    pub max_applicability_lease: Duration,
}

impl Default for EnforcerConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            max_applicability_lease: Duration::from_secs(60),
        }
    }
}

enum PluginSource {
    Hub {
        hub: Arc<ClientHub>,
        selector: GtsPluginSelector,
    },
    Fixed(Arc<dyn OperationGovernancePluginClientV1>),
}

type CacheKey = (String, Uuid);
/// Applicability per operation family and tenant, with the instant its lease ends.
type ApplicabilityCache = HashMap<CacheKey, (Instant, Arc<Applicability>)>;

/// Enforces Operation Governance for one Consuming Gear.
pub struct GovernanceEnforcer {
    source: PluginSource,
    config: EnforcerConfig,
    applicability: RwLock<ApplicabilityCache>,
    schemas: RwLock<HashMap<&'static str, Arc<FactSchema>>>,
}

impl GovernanceEnforcer {
    /// An Enforcer that selects the governance plugin through types-registry,
    /// by the configured vendor and the lowest priority, like a gateway gear.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, config: EnforcerConfig) -> Self {
        Self::with_source(
            PluginSource::Hub {
                hub,
                selector: GtsPluginSelector::new(),
            },
            config,
        )
    }

    /// An Enforcer bound to one plugin instance, for tests and embedding.
    #[must_use]
    pub fn with_plugin(
        plugin: Arc<dyn OperationGovernancePluginClientV1>,
        config: EnforcerConfig,
    ) -> Self {
        Self::with_source(PluginSource::Fixed(plugin), config)
    }

    fn with_source(source: PluginSource, config: EnforcerConfig) -> Self {
        Self {
            source,
            config,
            applicability: RwLock::new(HashMap::new()),
            schemas: RwLock::new(HashMap::new()),
        }
    }

    /// Check one attempt of the Governed Operation `P::Facts` on `resource`.
    ///
    /// Only [`GovernanceOutcome::Allowed`] lets the operation run.
    pub async fn check<P: FactProvider>(
        &self,
        ctx: &SecurityContext,
        resource: GovernedResource,
        mode: CheckMode,
        provider: &P,
    ) -> GovernanceOutcome {
        let schema = match self.schema::<P::Facts>() {
            Ok(schema) => schema,
            Err(e) => return invalid(e.to_string()),
        };
        let operation = schema.type_id();

        let plugin = match self.plugin().await {
            Ok(plugin) => plugin,
            Err(e) => return unavailable(&e),
        };

        let applicability = match self
            .applicability_of(plugin.as_ref(), ctx, operation, resource.tenant_id)
            .await
        {
            Ok(a) => a,
            Err(e) => return unavailable(&e),
        };
        if !applicability.governed {
            tracing::trace!(operation, "not governed: fast path");
            return GovernanceOutcome::Allowed;
        }

        let mut writer = FactWriter::new(&schema);
        let need = current_ids(&schema, &applicability.facts_read);
        if let Err(e) = load(&schema, provider, &need, &mut writer).await {
            return fact_failure(&e);
        }

        for round in 0..=1_u8 {
            let request = request_from(operation, &resource, mode, round, &writer);
            let response = match plugin.check(ctx, &request).await {
                Ok(r) => r,
                Err(e) => return unavailable(&e),
            };
            match response {
                CheckResponse::Decided(outcome) => return outcome,
                CheckResponse::NeedFacts { missing } if round == 0 => {
                    self.forget(operation, resource.tenant_id);
                    let missing = current_ids(&schema, &missing);
                    let unknown: Vec<&String> = missing
                        .iter()
                        .filter(|f| schema.traits(f).is_none())
                        .collect();
                    if !unknown.is_empty() {
                        return GovernanceOutcome::Unavailable {
                            message: format!(
                                "policies read facts `{operation}` does not declare: {unknown:?}"
                            ),
                            retryable: false,
                        };
                    }
                    let still: FactSet = missing.into_iter().filter(|f| !writer.has(f)).collect();
                    if let Err(e) = load(&schema, provider, &still, &mut writer).await {
                        return fact_failure(&e);
                    }
                }
                CheckResponse::NeedFacts { missing } => {
                    return GovernanceOutcome::Unavailable {
                        message: format!("facts still missing after one request: {missing:?}"),
                        retryable: false,
                    };
                }
            }
        }
        GovernanceOutcome::Unavailable {
            message: "governance check did not decide".to_owned(),
            retryable: false,
        }
    }

    fn schema<T: gts::GtsSchema>(&self) -> Result<Arc<FactSchema>, FactError> {
        if let Some(schema) = self.schemas.read().get(T::TYPE_ID) {
            return Ok(schema.clone());
        }
        let schema = Arc::new(FactSchema::of::<T>()?);
        self.schemas.write().insert(T::TYPE_ID, schema.clone());
        Ok(schema)
    }

    async fn plugin(&self) -> Result<Arc<dyn OperationGovernancePluginClientV1>, OgError> {
        match &self.source {
            PluginSource::Fixed(plugin) => Ok(plugin.clone()),
            PluginSource::Hub { hub, selector } => {
                let instance_id = selector
                    .get_or_init(|| resolve_plugin(hub, &self.config.vendor))
                    .await?;
                hub.try_get_scoped::<dyn OperationGovernancePluginClientV1>(&ClientScope::gts_id(
                    instance_id.as_ref(),
                ))
                .ok_or_else(|| {
                    OgError::Unavailable(format!(
                        "governance plugin `{instance_id}` is not registered yet"
                    ))
                })
            }
        }
    }

    async fn applicability_of(
        &self,
        plugin: &dyn OperationGovernancePluginClientV1,
        ctx: &SecurityContext,
        operation: &str,
        tenant_id: Uuid,
    ) -> Result<Arc<Applicability>, OgError> {
        let key = (operation_family(operation), tenant_id);
        // Hit: reuse the answer until its lease ends.
        if let Some((expires_at, cached)) = self.applicability.read().get(&key)
            && Instant::now() < *expires_at
        {
            return Ok(cached.clone());
        }
        // Miss or expired lease: ask the backend, and cache the answer for
        // the lease it grants, capped by the configured maximum.
        let fresh = Arc::new(plugin.applicability(ctx, operation).await?);
        let lease = Duration::from_millis(fresh.lease_ms).min(self.config.max_applicability_lease);
        let mut cache = self.applicability.write();
        if lease.is_zero() {
            cache.remove(&key);
        } else {
            cache.insert(key, (Instant::now() + lease, fresh.clone()));
        }
        Ok(fresh)
    }

    fn forget(&self, operation: &str, tenant_id: Uuid) {
        self.applicability
            .write()
            .remove(&(operation_family(operation), tenant_id));
    }
}

async fn resolve_plugin(hub: &ClientHub, vendor: &str) -> Result<String, OgError> {
    let registry = hub
        .get::<dyn TypesRegistryClient>()
        .map_err(|e| OgError::Unavailable(format!("types registry: {e}")))?;
    let plugin_type_id = OperationGovernancePluginSpecV1::gts_type_id().clone();
    let instances = registry
        .list_instances(InstanceQuery::new().with_pattern(format!("{plugin_type_id}*")))
        .await
        .map_err(|e| OgError::Unavailable(format!("types registry: {e}")))?;
    choose_plugin_instance::<OperationGovernancePluginSpecV1>(
        vendor,
        instances.iter().map(|e| (e.id.as_ref(), &e.object)),
    )
    .map_err(|e| OgError::Unavailable(format!("no governance plugin for vendor `{vendor}`: {e}")))
}

/// Facts read by policies, under the IDs the gear's current type version uses.
/// A Fact no version declares is kept as is, so the backend can report it.
fn current_ids(schema: &FactSchema, facts: &FactSet) -> FactSet {
    facts
        .iter()
        .map(|f| schema.current_id(f).unwrap_or(f).to_owned())
        .collect()
}

async fn load<P: FactProvider>(
    schema: &FactSchema,
    provider: &P,
    need: &FactSet,
    writer: &mut FactWriter<'_>,
) -> Result<(), FactError> {
    let declared: FactSet = need
        .iter()
        .filter(|f| schema.traits(f).is_some())
        .cloned()
        .collect();
    for (_, tier_facts) in schema.by_tier(&declared) {
        provider.provide(&tier_facts, writer).await?;
        for fact in &tier_facts {
            if !writer.has(fact) {
                writer.unavailable(fact, "the fact provider did not write it");
            }
        }
    }
    Ok(())
}

fn request_from(
    operation: &str,
    resource: &GovernedResource,
    mode: CheckMode,
    round: u8,
    writer: &FactWriter<'_>,
) -> GovernanceCheckRequest {
    let (facts, unavailable) = writer.snapshot();
    GovernanceCheckRequest {
        operation: operation.to_owned(),
        resource: resource.clone(),
        facts,
        unavailable,
        mode,
        round,
    }
}

fn invalid(message: String) -> GovernanceOutcome {
    GovernanceOutcome::Invalid { message }
}

fn unavailable(e: &OgError) -> GovernanceOutcome {
    match e {
        OgError::Invalid(message) => invalid(message.clone()),
        OgError::Unavailable(message) => GovernanceOutcome::Unavailable {
            message: message.clone(),
            retryable: true,
        },
        OgError::Internal(message) => GovernanceOutcome::Unavailable {
            message: message.clone(),
            retryable: false,
        },
    }
}

fn fact_failure(e: &FactError) -> GovernanceOutcome {
    match e {
        FactError::Provider(message) => GovernanceOutcome::Unavailable {
            message: message.clone(),
            retryable: true,
        },
        // A gear defect: the provider wrote an undeclared or malformed Fact,
        // or the type's schema is broken.
        FactError::Undeclared(_) | FactError::Invalid { .. } | FactError::Schema(_) => {
            invalid(e.to_string())
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "enforcer_tests.rs"]
mod enforcer_tests;
