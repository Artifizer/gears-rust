//! Fakes and wiring shared by the service and REST tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers: a failed expectation is the test failing"
)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::{
    AuthZResolverApi, PolicyEnforcer,
    constraints::{Constraint, EqPredicate, Predicate},
    models::{EvaluationRequest, EvaluationResponse, EvaluationResponseContext},
};
use file_parser_sdk::{FileParserClientError, FileParserClientV1};
use prompt_scanner_sdk::{
    BackendOutcome, Detection, PromptScannerError, ScanInput, ScanPhase, ScannerBackendClient,
    ScannerBackendSpecV1, Severity, TenantPolicy, UnscoredReason, VerdictAction,
};
use sea_orm_migration::MigratorTrait;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::gts::PluginV1;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
use uuid::Uuid;

use crate::config::FailMode;
use crate::domain::metrics::{EvidenceMetricsPort, NoopMetrics, NotStored};
use crate::domain::repos::EvidenceRepository;
use crate::domain::service::{DbProvider, Service, ServiceSettings};
use crate::infra::storage::OrmEvidenceRepository;
use crate::infra::storage::migrations::Migrator;

pub type TestService = Service<OrmEvidenceRepository>;

/// Permissive PDP fake: permits every request, scoped to the caller's own
/// tenant via an `Eq(owner_tenant_id, ctx.subject_tenant_id())` constraint.
pub struct MockAuthZResolver;

#[async_trait]
impl AuthZResolverApi for MockAuthZResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant_str = request
            .subject
            .properties
            .get("tenant_id")
            .and_then(serde_json::Value::as_str)
            .expect("MockAuthZResolver: subject.properties[tenant_id] missing");
        let tenant = Uuid::parse_str(tenant_str).expect("MockAuthZResolver: bad tenant UUID");

        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::Eq(EqPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        tenant,
                    ))],
                }],
                deny_reason: None,
            },
        })
    }
}

/// A PDP that denies every request whose action is in `deny`, and permits
/// the rest like [`MockAuthZResolver`].
pub struct DenyActions(pub &'static [&'static str]);

#[async_trait]
impl AuthZResolverApi for DenyActions {
    async fn evaluate(
        &self,
        ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        if self.0.contains(&request.action.name.as_str()) {
            return Ok(EvaluationResponse {
                decision: false,
                context: EvaluationResponseContext {
                    constraints: Vec::new(),
                    deny_reason: None,
                },
            });
        }
        MockAuthZResolver.evaluate(ctx, request).await
    }
}

/// An always-permit PDP that returns **no constraints**: an unconstrained
/// allow.
pub struct UnconstrainedAllowResolver;

#[async_trait]
impl AuthZResolverApi for UnconstrainedAllowResolver {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        _request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: Vec::new(),
                deny_reason: None,
            },
        })
    }
}

pub fn mock_enforcer() -> PolicyEnforcer {
    PolicyEnforcer::new(Arc::new(MockAuthZResolver))
}

pub fn ctx_for_tenant(tenant_id: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant_id)
        .build()
        .expect("valid SecurityContext")
}

pub async fn in_memory_db() -> Arc<DbProvider> {
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db("sqlite::memory:", opts)
        .await
        .expect("connect in-memory sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .map_err(|e| e.to_string())
        .expect("run migrations");
    Arc::new(DBProvider::new(db))
}

/// A backend fake reporting one finding with a configurable category and
/// action.
pub struct FakeBackend {
    pub category: String,
    pub action: VerdictAction,
}

/// The version `FakeBackend` reports.
pub const FAKE_VERSION: &str = "9.9.9";

#[async_trait]
impl ScannerBackendClient for FakeBackend {
    async fn detect(
        &self,
        _ctx: &SecurityContext,
        input: &ScanInput,
    ) -> Result<BackendOutcome, PromptScannerError> {
        let end = u32::try_from(input.payload.len().min(5)).unwrap();
        Ok(BackendOutcome::Detections(vec![
            Detection::new(self.category.clone(), Severity::Medium, 0.95, self.action)
                .with_span(prompt_scanner_sdk::Span { start: 0, end }),
        ]))
    }

    fn version(&self) -> Option<String> {
        Some(FAKE_VERSION.to_owned())
    }
}

/// A backend fake that never responds.
pub struct SlowBackend;

#[async_trait]
impl ScannerBackendClient for SlowBackend {
    async fn detect(
        &self,
        _ctx: &SecurityContext,
        _input: &ScanInput,
    ) -> Result<BackendOutcome, PromptScannerError> {
        tokio::time::sleep(Duration::from_hours(1)).await;
        Ok(BackendOutcome::Detections(Vec::new()))
    }
}

/// A backend fake that always errors.
pub struct FailingBackend;

#[async_trait]
impl ScannerBackendClient for FailingBackend {
    async fn detect(
        &self,
        _ctx: &SecurityContext,
        _input: &ScanInput,
    ) -> Result<BackendOutcome, PromptScannerError> {
        Err(PromptScannerError::internal("model crashed"))
    }
}

/// Defers every inline run. On the deferred run (`ScanInput::phase` is
/// `Deferred`) it answers with one `Block` finding (`pii.person`) — or
/// defers again when `always_defer`. Records the phase of every call.
#[derive(Default)]
pub struct DeferInlineBackend {
    pub always_defer: bool,
    pub phases: StdMutex<Vec<ScanPhase>>,
}

impl DeferInlineBackend {
    pub fn phases(&self) -> Vec<ScanPhase> {
        self.phases.lock().unwrap().clone()
    }
}

#[async_trait]
impl ScannerBackendClient for DeferInlineBackend {
    async fn detect(
        &self,
        _ctx: &SecurityContext,
        input: &ScanInput,
    ) -> Result<BackendOutcome, PromptScannerError> {
        self.phases.lock().unwrap().push(input.phase);
        if self.always_defer || !input.is_deferred_run() {
            return Ok(BackendOutcome::Defer);
        }
        Ok(BackendOutcome::Detections(vec![Detection::new(
            "pii.person",
            Severity::High,
            0.95,
            VerdictAction::Block,
        )]))
    }
}

/// A backend fake reporting itself unavailable, as the ONNX scanner does
/// when its model is not loaded.
pub struct UnavailableBackend;

#[async_trait]
impl ScannerBackendClient for UnavailableBackend {
    async fn detect(
        &self,
        _ctx: &SecurityContext,
        _input: &ScanInput,
    ) -> Result<BackendOutcome, PromptScannerError> {
        Err(PromptScannerError::BackendUnavailable {
            backend: "unavailable_vendor".to_owned(),
            reason: "model_unavailable".to_owned(),
        })
    }
}

/// A `file-parser` fake: the bytes as UTF-8 text, or a configured error, or
/// a hang.
#[derive(Clone)]
pub enum FakeFileParser {
    Text,
    Fail(FileParserClientError),
    Hang,
}

#[async_trait]
impl FileParserClientV1 for FakeFileParser {
    async fn parse_bytes(
        &self,
        _ctx: &SecurityContext,
        request: file_parser_sdk::ParseBytesRequest,
    ) -> Result<file_parser_sdk::ParsedText, FileParserClientError> {
        match self {
            Self::Text => Ok(file_parser_sdk::ParsedText {
                markdown: String::from_utf8_lossy(&request.bytes).into_owned(),
            }),
            Self::Fail(e) => Err(e.clone()),
            Self::Hang => {
                tokio::time::sleep(Duration::from_hours(1)).await;
                Err(FileParserClientError::Internal("unreachable".to_owned()))
            }
        }
    }
}

pub fn register_file_parser(hub: &ClientHub, parser: FakeFileParser) {
    hub.register::<dyn FileParserClientV1>(Arc::new(parser));
}

/// Registers a scanner backend plugin instance in the registry list and the
/// `ClientHub`, mirroring what a real plugin gear's `init()` does.
pub fn register_backend(
    hub: &ClientHub,
    registry: &mut Vec<serde_json::Value>,
    vendor: &str,
    client: Arc<dyn ScannerBackendClient>,
) -> String {
    let (instance_id, instance_json) = PluginV1::<ScannerBackendSpecV1>::build_registration(
        &format!("cf.test.{vendor}.detector.v1"),
        vendor,
        100,
    )
    .expect("build_registration");
    registry.push(instance_json);
    hub.register_scoped::<dyn ScannerBackendClient>(ClientScope::gts_id(&instance_id), client);
    instance_id.to_string()
}

/// Registers the mock types-registry holding `instances`.
pub fn register_registry(hub: &ClientHub, instances: Vec<serde_json::Value>) {
    let mock_registry: Arc<dyn TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new().with_instances(instances.into_iter().map(|obj| {
            let id = obj["id"].as_str().expect("instance has id").to_owned();
            make_test_instance(&id, obj)
        })),
    );
    hub.register::<dyn TypesRegistryClient>(mock_registry);
}

/// A hub whose registry holds one backend per `(vendor, client)`.
pub fn hub_with(backends: Vec<(&str, Arc<dyn ScannerBackendClient>)>) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    let mut instances = Vec::new();
    for (vendor, client) in backends {
        register_backend(&hub, &mut instances, vendor, client);
    }
    register_registry(&hub, instances);
    hub
}

/// A writable in-memory `PolicyProvider`, standing in for a provider plugin:
/// prompt-scanner's own fallback is read-only.
#[derive(Default)]
pub struct InMemoryPolicyProvider(tokio::sync::Mutex<HashMap<Uuid, TenantPolicy>>);

#[async_trait]
impl prompt_scanner_sdk::PolicyProvider for InMemoryPolicyProvider {
    async fn get_policy(
        &self,
        _ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Option<TenantPolicy>, PromptScannerError> {
        Ok(self.0.lock().await.get(&tenant_id).cloned())
    }

    async fn put_policy(
        &self,
        _ctx: &SecurityContext,
        policy: TenantPolicy,
    ) -> Result<TenantPolicy, PromptScannerError> {
        self.0.lock().await.insert(policy.tenant_id, policy.clone());
        Ok(policy)
    }
}

pub fn test_settings() -> ServiceSettings {
    ServiceSettings {
        fail_mode: FailMode::Closed,
        backend_timeout: Duration::from_millis(200),
        deferred_backend_timeout: Duration::from_millis(500),
        attachment_parse_timeout: Duration::from_millis(500),
        ..ServiceSettings::default()
    }
}

/// Everything a test may want to vary.
pub struct Wiring<R: EvidenceRepository + 'static> {
    pub db: Arc<DbProvider>,
    pub hub: Arc<ClientHub>,
    pub repo: Arc<R>,
    pub provider: Option<Arc<dyn prompt_scanner_sdk::PolicyProvider>>,
    pub enforcer: PolicyEnforcer,
    pub metrics: Arc<dyn EvidenceMetricsPort>,
    pub settings: ServiceSettings,
}

impl Wiring<OrmEvidenceRepository> {
    pub async fn new(hub: Arc<ClientHub>) -> Self {
        Self::over(in_memory_db().await, hub)
    }

    pub fn over(db: Arc<DbProvider>, hub: Arc<ClientHub>) -> Self {
        Self {
            db,
            hub,
            repo: Arc::new(OrmEvidenceRepository::new()),
            provider: Some(Arc::new(InMemoryPolicyProvider::default())),
            enforcer: mock_enforcer(),
            metrics: Arc::new(NoopMetrics),
            settings: test_settings(),
        }
    }
}

impl<R: EvidenceRepository + 'static> Wiring<R> {
    pub fn with_repo<R2: EvidenceRepository + 'static>(self, repo: Arc<R2>) -> Wiring<R2> {
        Wiring {
            db: self.db,
            hub: self.hub,
            repo,
            provider: self.provider,
            enforcer: self.enforcer,
            metrics: self.metrics,
            settings: self.settings,
        }
    }

    /// The service, with its evidence writer running on the test runtime.
    pub fn build(self) -> Service<R> {
        let (service, writer) = Service::new(
            self.db,
            self.hub,
            self.repo,
            self.provider,
            self.enforcer,
            self.metrics,
            self.settings,
        );
        tokio::spawn(writer.run(tokio_util::sync::CancellationToken::new()));
        service
    }
}

pub async fn make_service(hub: Arc<ClientHub>) -> TestService {
    Wiring::new(hub).await.build()
}

/// Counts what the service reports, for assertions.
#[derive(Default)]
pub struct CountingMetrics {
    pub write_failures: AtomicU64,
    pub deleted: AtomicU64,
    pub backlog: AtomicU64,
    pub queue_full: AtomicU64,
    pub jobs_dropped: AtomicU64,
    pub deferred_failures: AtomicUsize,
    pub required_deferred_failures: AtomicUsize,
    pub unscored: StdMutex<Vec<UnscoredReason>>,
}

impl EvidenceMetricsPort for CountingMetrics {
    fn scan_completed(&self, _flagged: bool) {}
    fn findings_not_stored(&self, reason: NotStored, count: u64) {
        if reason == NotStored::QueueFull {
            self.queue_full.fetch_add(count, Ordering::SeqCst);
        }
    }
    fn evidence_write_failed(&self) {
        self.write_failures.fetch_add(1, Ordering::SeqCst);
    }
    fn retention_deleted(&self, rows: u64) {
        self.deleted.fetch_add(rows, Ordering::SeqCst);
    }
    fn retention_backlog(&self, rows: u64) {
        self.backlog.store(rows, Ordering::SeqCst);
    }
    fn jobs_dropped(&self, count: u64) {
        self.jobs_dropped.fetch_add(count, Ordering::SeqCst);
    }
    fn deferred_backend_failed(&self, required: bool) {
        self.deferred_failures.fetch_add(1, Ordering::SeqCst);
        if required {
            self.required_deferred_failures
                .fetch_add(1, Ordering::SeqCst);
        }
    }
    fn backend_unscored(&self, reason: UnscoredReason) {
        self.unscored.lock().unwrap().push(reason);
    }
}
