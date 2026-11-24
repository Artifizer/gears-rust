//! prompt-scanner gear.

use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use prompt_scanner_sdk::PromptScannerClientV1;
use sea_orm_migration::MigrationTrait;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::lifecycle::ReadySignal;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use tracing::{info, warn};

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};

use crate::api::rest::routes;
use crate::config::{FailMode, PromptScannerConfig};
use crate::domain::local_client::PromptScannerLocalClient;
use crate::domain::metrics::EvidenceMetricsPort;
use crate::domain::policy_provider::ConfigPolicyProvider;
use crate::domain::repos::{EvidenceMaintenance, EvidenceRepository};
use crate::domain::retention::EvidenceMaintenanceJobs;
use crate::domain::service::{EvidenceWriter, Service, ServiceSettings};
use crate::infra::metrics::EvidenceMetricsMeter;
use crate::infra::storage::OrmEvidenceRepository;

/// The gear's name: its `#[toolkit::gear]` name and its advisory-lock
/// namespace.
pub(crate) const GEAR_NAME: &str = "prompt-scanner";

/// How long shutdown waits for queued writes of flagged scans.
const PENDING_WRITES_TIMEOUT: Duration = Duration::from_secs(5);

/// Concrete `Service` type used with `SeaORM` repositories.
///
/// Lives in the composition root (`gear.rs`) so the domain layer stays
/// generic over the repository traits, per the DDD-light layering rules.
pub(crate) type ConcreteService = Service<OrmEvidenceRepository>;

/// Concrete maintenance jobs (retention, tenant deletion).
pub(crate) type ConcreteMaintenance = EvidenceMaintenanceJobs<OrmEvidenceRepository>;

/// Everything `init` builds and `serve` runs.
struct Runtime {
    service: Arc<ConcreteService>,
    maintenance: Arc<ConcreteMaintenance>,
    /// Taken by `serve`.
    writer: Mutex<Option<EvidenceWriter<OrmEvidenceRepository>>>,
    job_poll_interval: Duration,
    jobs_per_tick: usize,
    retention_interval: Duration,
}

#[toolkit::gear(
    name = "prompt-scanner",
    deps = [authz_resolver, types_registry, file_parser],
    capabilities = [db, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "10s", await_ready)
)]
#[derive(Default)]
pub struct PromptScanner {
    runtime: OnceLock<Runtime>,
}

#[async_trait]
impl Gear for PromptScanner {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: PromptScannerConfig = ctx.config_or_default()?;
        cfg.validate().context("invalid prompt_scanner config")?;
        info!(
            fail_mode = ?cfg.fail_mode,
            backend_timeout = ?cfg.backend_timeout,
            job_queue_capacity = cfg.job_queue_capacity,
            max_retention = ?cfg.evidence.max_retention,
            max_evidence_retention = ?cfg.evidence.max_evidence_retention,
            retention_interval = ?cfg.evidence.retention_interval,
            "Loaded prompt_scanner config"
        );

        let db: Arc<DBProvider<toolkit_db::DbError>> = Arc::new(ctx.db_required()?);

        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .context("failed to get AuthZ resolver")?;
        let policy_enforcer = PolicyEnforcer::new(authz);

        let repo = Arc::new(OrmEvidenceRepository::new());
        let metrics_scope =
            opentelemetry::InstrumentationScope::builder(Self::MODULE_NAME.to_owned()).build();
        let metrics: Arc<dyn EvidenceMetricsPort> = Arc::new(EvidenceMetricsMeter::new(
            &opentelemetry::global::meter_with_scope(metrics_scope),
        ));

        let default_policy_provider = default_policy_provider(&cfg)?;

        let (service, writer) = Service::new(
            Arc::clone(&db),
            ctx.client_hub(),
            Arc::clone(&repo),
            default_policy_provider,
            policy_enforcer,
            Arc::clone(&metrics),
            ServiceSettings::from_config(&cfg),
        );
        let service = Arc::new(service);
        let maintenance = Arc::new(EvidenceMaintenanceJobs::new(
            db,
            repo,
            metrics,
            cfg.evidence.retention_batch_size,
        ));

        self.runtime
            .set(Runtime {
                service: Arc::clone(&service),
                maintenance,
                writer: Mutex::new(Some(writer)),
                job_poll_interval: cfg.job_poll_interval,
                jobs_per_tick: cfg.jobs_per_tick,
                retention_interval: cfg.evidence.retention_interval,
            })
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let api: Arc<dyn PromptScannerClientV1> = Arc::new(PromptScannerLocalClient::new(service));
        ctx.client_hub().register::<dyn PromptScannerClientV1>(api);

        Ok(())
    }
}

/// The config-backed provider for `fallback_policy`, validated like a policy
/// written through the API; `None`, with a warning, when it is unset.
fn default_policy_provider(
    cfg: &PromptScannerConfig,
) -> anyhow::Result<Option<Arc<dyn prompt_scanner_sdk::PolicyProvider>>> {
    let Some(fallback) = cfg.fallback_policy.clone() else {
        warn_without_fallback(cfg.fail_mode);
        return Ok(None);
    };
    let policy = fallback.into_template();
    ConcreteService::validate_policy(&policy).context("invalid fallback_policy")?;
    Ok(Some(Arc::new(ConfigPolicyProvider::new(Some(policy)))))
}

/// No provider plugin may register, and no `fallback_policy` is set: say what
/// will happen.
fn warn_without_fallback(fail_mode: FailMode) {
    match fail_mode {
        FailMode::Closed => warn!(
            "prompt_scanner: no fallback_policy configured; unless a policy provider plugin \
             registers, every scan fails with PolicyUnavailable (fail_mode = closed)"
        ),
        FailMode::Open => warn!(
            "prompt_scanner: no fallback_policy configured; unless a policy provider plugin \
             registers, every tenant gets the permissive default policy and nothing is scanned \
             (fail_mode = open)"
        ),
    }
}

impl DatabaseCapability for PromptScanner {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing prompt_scanner database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for PromptScanner {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering prompt_scanner REST routes");
        let service = Arc::clone(&self.runtime()?.service);
        let router = routes::register_routes(router, openapi, service);
        info!("prompt_scanner REST routes registered successfully");
        Ok(router)
    }
}

impl PromptScanner {
    fn runtime(&self) -> anyhow::Result<&Runtime> {
        self.runtime
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// Stateful entry: runs the evidence writer, the async job worker and
    /// the retention job as separate tasks until cancelled. On shutdown it
    /// drops (and counts) jobs still queued, then waits briefly for queued
    /// evidence writes.
    async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        let rt = self.runtime()?;
        let service = Arc::clone(&rt.service);
        let writer = rt
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .context("prompt_scanner serve started twice")?;

        let writer_cancel = CancellationToken::new();
        let writer_task = tokio::spawn(writer.run(writer_cancel.clone()));
        let jobs_task = tokio::spawn(run_job_loop(
            Arc::clone(&service),
            cancel.clone(),
            rt.job_poll_interval,
            rt.jobs_per_tick,
        ));
        let retention_task = tokio::spawn(run_retention_loop(
            Arc::clone(&rt.maintenance),
            cancel.clone(),
            rt.retention_interval,
        ));

        ready.notify();
        cancel.cancelled().await;
        info!("prompt_scanner background tasks shutting down");

        join_task("job worker", jobs_task).await;
        join_task("retention", retention_task).await;
        service.drop_queued_jobs().await;
        flush_writes(&service).await;
        writer_cancel.cancel();
        join_task("evidence writer", writer_task).await;
        Ok(())
    }
}

/// Give queued evidence writes a bounded time to finish.
async fn flush_writes(service: &ConcreteService) {
    if tokio::time::timeout(PENDING_WRITES_TIMEOUT, service.wait_for_pending_writes())
        .await
        .is_err()
    {
        warn!("shutdown: queued writes of flagged scans did not finish in time");
    }
}

/// Wait for a background task, logging a panic.
async fn join_task(name: &'static str, task: tokio::task::JoinHandle<()>) {
    if let Err(e) = task.await {
        warn!(task = name, error = %e, "prompt_scanner background task ended abnormally");
    }
}

/// The async job worker: every `poll_interval`, run at most `per_tick`
/// queued jobs, until cancelled.
pub(crate) async fn run_job_loop<SR: EvidenceRepository + 'static>(
    service: Arc<Service<SR>>,
    cancel: CancellationToken,
    poll_interval: Duration,
    per_tick: usize,
) {
    let mut ticks = tokio::time::interval(poll_interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            _ = ticks.tick() => {
                service.drain_jobs(per_tick, &cancel).await;
            }
        }
    }
}

/// The retention job: one run every `interval`, until cancelled.
pub(crate) async fn run_retention_loop<M: EvidenceMaintenance + 'static>(
    maintenance: Arc<EvidenceMaintenanceJobs<M>>,
    cancel: CancellationToken,
    interval: Duration,
) {
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            _ = ticks.tick() => {
                if let Err(e) = maintenance.run_retention(&cancel).await {
                    warn!(error = %e, "retention run failed; the next run continues");
                }
            }
        }
    }
}
