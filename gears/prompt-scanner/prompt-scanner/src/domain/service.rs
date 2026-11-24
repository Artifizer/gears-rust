//! Domain service for the prompt-scanner gear.
//!
//! Orchestrates the fan-out-to-all-enabled-backends scan flow described in
//! `docs/DESIGN.md`:
//!
//! 1. Authorize the caller (PDP, `CREATE` on its own tenant's scan records)
//!    before touching the content.
//! 2. Resolve the tenant's [`TenantPolicy`] (`enabled_backends`).
//! 3. Query `types-registry` for **every** registered `ScannerBackendSpecV1`
//!    instance, filter by `enabled_backends`, and resolve each via
//!    `ClientHub::try_get_scoped::<dyn ScannerBackendClient>(&ClientScope::gts_id(...))`.
//! 4. Call every resolved backend **concurrently**, with a per-backend
//!    timeout. A backend that has no usable instance, times out or errors
//!    fails the whole scan when the tenant policy marks it `required`.
//!    Otherwise the scan goes on without it (degraded coverage): it is listed
//!    in `ScanVerdict::unscored_backends` with a stable reason, logged, and
//!    counted (`EvidenceMetricsPort::backend_unscored`), and never changes
//!    the verdict action. A backend may instead return
//!    [`BackendOutcome::Defer`] — see "Async deferral" below.
//! 5. Aggregate findings into one verdict per `domain::policy::aggregate_verdict`.
//! 6. Return the verdict, then hand a flagged scan's rows (one per finding)
//!    to the evidence writer: a bounded queue drained by one background
//!    task. A full queue or a failed write loses that scan's evidence and is
//!    counted; it never fails or delays the scan.
//!
//! This deliberately does **not** use `toolkit::plugins::choose_plugin_instance`
//! — that helper picks a single "best" instance by vendor+priority, which is
//! the wrong shape here (`prompt-scanner` wants "all enabled", not "one").
//!
//! # Async jobs
//!
//! `scan_async` and deferred backends share one bounded in-memory queue
//! (`job_queue_capacity`), drained by the gear's worker task a bounded number
//! of jobs per tick ([`Service::drain_jobs`]). A full queue refuses
//! `scan_async` with `Overloaded`. Job statuses live in memory and are
//! forgotten `job_status_ttl` after the job finishes. Queued jobs are lost
//! on restart; a durable queue is deliberately deferred.
//!
//! # Async deferral for oversized inputs
//!
//! A backend that decides a given input is too expensive to evaluate
//! synchronously returns [`BackendOutcome::Defer`]. On the synchronous path
//! the deferring backend is queued as a `ScanJob::DeferredBackend` under the
//! scan's existing `trace_id`; the other backends' findings still aggregate
//! into the verdict returned now, which lists the deferring backend as
//! unscored (`deferred`). The worker later calls just that backend again,
//! with [`ScanPhase::Deferred`] on the input (so the backend knows not to
//! defer again) and `deferred_backend_timeout`, re-runs `aggregate_verdict`
//! across the scan's findings plus the new ones, and appends the new findings as further
//! rows. Rows are never updated: a read takes the scan's verdict as the
//! highest `action` across its rows. A deferred call that fails, times out
//! or defers again is counted and logged; inside a `scan_async` job it fails
//! the job when the backend is required. See `docs/DESIGN.md`'s "Async
//! deferral for oversized inputs".

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::Utc;
use file_parser_sdk::{Detection, FileParserClientV1, ParseBytesRequest};
use prompt_scanner_sdk::{
    BackendOutcome, Detection as BackendDetection, Finding, InvalidScanRequest, MAX_PAYLOAD_BYTES,
    PolicyProvider, PolicyProviderSpecV1, PromptScannerError, ScanContent, ScanInput,
    ScanJobFailureCode, ScanJobHandle, ScanJobStatus, ScanPhase, ScanRequest, ScanVerdict,
    ScannerBackendClient, ScannerBackendSpecV1, TenantPolicy, UnscoredBackend, UnscoredReason,
    VerdictAction,
};
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tokio_util::task::task_tracker::TaskTrackerToken;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit_db::DBProvider;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use tracing::instrument;
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};
use uuid::Uuid;

use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use authz_resolver_sdk::{EnforcerError, PolicyEnforcer};

use crate::config::{EvidenceConfig, FailMode, PromptScannerConfig};
use crate::domain::error::DomainError;
use crate::domain::evidence::{BuiltRows, NotStored, ScanFacts, build_rows};
use crate::domain::metrics::EvidenceMetricsPort;
use crate::domain::policy::aggregate_verdict;
use crate::domain::repos::{EvidenceRepository, EvidenceRow};

pub(crate) type DbProvider = DBProvider<toolkit_db::DbError>;

/// Authorization resource types and their PEP-supported properties.
pub(crate) mod resources {
    use super::ResourceType;
    use toolkit_security::pep_properties;

    /// A stored scan (its `evidences` rows) and, for `get`, an async job.
    pub const SCAN_RECORD: ResourceType = ResourceType::from_static(
        "prompt_scanner.scan_record",
        &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
    );

    /// A tenant's scanning policy.
    pub const TENANT_POLICY: ResourceType = ResourceType::from_static(
        "prompt_scanner.tenant_policy",
        &[pep_properties::OWNER_TENANT_ID],
    );

    /// The operator view of registered scanner backends.
    pub const BACKEND: ResourceType =
        ResourceType::from_static("prompt_scanner.backend", &[pep_properties::OWNER_TENANT_ID]);
}

/// Actions. Scan records have no `list` (they are found by `trace_id` or
/// `correlation_id` only) and no `update` (rows are append-only).
pub(crate) mod actions {
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    pub const CREATE: &str = "create";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
}

/// Most rows a correlation lookup returns.
pub(crate) const CORRELATION_LOOKUP_LIMIT: u64 = 1_000;

/// `backend` reported for a `file-parser` timeout.
const FILE_PARSER: &str = "file-parser";

/// Limits on a tenant policy, so a policy (written through the API or served
/// by a provider plugin) cannot blow up per-scan work or the stored rows.
mod policy_limits {
    pub const MAX_BACKENDS: usize = 64;
    pub const MAX_ENTRIES: usize = 256;
    pub const MAX_NAME_CHARS: usize = 128;
    pub const MAX_SETTINGS_SCHEMA_CHARS: usize = 16;
    pub const MAX_RETENTION: std::time::Duration = std::time::Duration::from_hours(24 * 365 * 100);
}

/// A queued asynchronous job, processed by the gear's background worker
/// ([`Service::drain_jobs`]). Module-private: never crosses a layer
/// boundary, so it's exempt from `#[domain_model]`.
enum ScanJob {
    /// An explicit `scan_async` submission — run the full scan under a
    /// pre-allocated `trace_id`.
    Scan {
        job_id: Uuid,
        trace_id: Uuid,
        ctx: SecurityContext,
        request: Box<ScanRequest>,
    },
    /// A single backend's deferred contribution to an already-answered
    /// synchronous scan. Not tracked in `job_status`: the caller already has
    /// the verdict and reads `get_verdict` for updates.
    DeferredBackend {
        ctx: SecurityContext,
        backend: ResolvedBackend,
        scan: Arc<DeferredScan>,
    },
}

/// A scanner backend instance the fan-out calls.
#[derive(Clone)]
struct ResolvedBackend {
    vendor: String,
    /// Stamped on each finding as `detector`.
    gts_id: String,
    client: Arc<dyn ScannerBackendClient>,
}

/// What a scan's deferred backends share: the input, the facts every row
/// repeats, and the findings and ordinals already used, so appended rows
/// continue them.
struct DeferredScan {
    input: ScanInput,
    facts: ScanFacts,
    policy: TenantPolicy,
    /// The CREATE scope the scan was authorized under.
    scope: AccessScope,
    state: Mutex<DeferredState>,
}

struct DeferredState {
    /// The scan's findings after aggregation, in order.
    considered: Vec<Finding>,
    next_ordinal: u16,
}

/// What the inline fan-out produced.
struct BackendRun {
    /// Findings of the backends that answered.
    findings: Vec<Finding>,
    /// Backends that deferred, to run in the background.
    deferred: Vec<ResolvedBackend>,
    /// Backends that did not contribute: non-required ones that failed, and
    /// every one that deferred.
    unscored: Vec<UnscoredBackend>,
}

impl BackendRun {
    fn empty() -> Self {
        Self {
            findings: Vec::new(),
            deferred: Vec::new(),
            unscored: Vec::new(),
        }
    }

    /// List `backend` as unscored for `reason`, once.
    fn unscored_push(&mut self, backend: &str, reason: UnscoredReason) {
        if !self
            .unscored
            .iter()
            .any(|u| u.backend == backend && u.reason == reason)
        {
            self.unscored.push(UnscoredBackend::new(backend, reason));
        }
    }
}

/// A scan's backends that deferred, to run in the background.
struct Deferred {
    backends: Vec<ResolvedBackend>,
    scan: Arc<DeferredScan>,
}

/// One async job's status, for `get_job_status`.
#[derive(Clone, Copy)]
struct JobEntry {
    /// The tenant that queued it: a job is only visible to its own tenant.
    tenant_id: Uuid,
    status: ScanJobStatus,
    /// When it finished; `None` while queued or running.
    finished_at: Option<Instant>,
}

/// A flagged scan's rows, on their way to the writer. The token keeps
/// [`Service::wait_for_pending_writes`] waiting until the write is done.
struct PendingWrite {
    scope: AccessScope,
    rows: Vec<EvidenceRow>,
    _token: TaskTrackerToken,
}

/// The fields of a plugin instance the gateway reads: `PluginV1`'s header.
#[derive(Deserialize)]
struct PluginHeader {
    vendor: String,
    priority: i16,
}

/// A registered plugin instance, its header parsed or not.
struct RegisteredPlugin {
    gts_id: String,
    header: Result<PluginHeader, serde_json::Error>,
}

/// A registered scanner backend plugin instance, for `GET
/// /prompt-scanner/v1/backends`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct BackendInstanceInfo {
    pub gts_id: String,
    pub vendor: String,
    pub priority: i16,
    /// Whether a scoped `ClientHub` entry is currently registered — i.e.
    /// whether the gateway could actually call this instance right now.
    pub available: bool,
}

/// What `Service::new` needs besides its collaborators.
#[derive(Debug, Clone)]
pub struct ServiceSettings {
    pub fail_mode: FailMode,
    pub backend_timeout: Duration,
    pub deferred_backend_timeout: Duration,
    pub attachment_parse_timeout: Duration,
    pub job_queue_capacity: usize,
    pub job_status_ttl: Duration,
    pub evidence: EvidenceConfig,
}

impl ServiceSettings {
    /// The service's share of the gear config.
    #[must_use]
    pub fn from_config(cfg: &PromptScannerConfig) -> Self {
        Self {
            fail_mode: cfg.fail_mode,
            backend_timeout: cfg.backend_timeout,
            deferred_backend_timeout: cfg.deferred_backend_timeout,
            attachment_parse_timeout: cfg.attachment_parse_timeout,
            job_queue_capacity: cfg.job_queue_capacity,
            job_status_ttl: cfg.job_status_ttl,
            evidence: cfg.evidence.clone(),
        }
    }
}

impl Default for ServiceSettings {
    fn default() -> Self {
        Self::from_config(&PromptScannerConfig::default())
    }
}

/// Domain service for prompt-scanner.
#[domain_model]
pub struct Service<SR>
where
    SR: EvidenceRepository + 'static,
{
    db: Arc<DbProvider>,
    hub: Arc<ClientHub>,
    repo: Arc<SR>,
    /// The policy source when no `PolicyProviderSpecV1` plugin instance is
    /// registered: the gear's `fallback_policy`. `None` when that is unset;
    /// what happens then is `fail_mode`'s call.
    default_policy_provider: Option<Arc<dyn PolicyProvider>>,
    policy_enforcer: PolicyEnforcer,
    settings: ServiceSettings,
    jobs_tx: mpsc::Sender<ScanJob>,
    /// Only the worker takes from it.
    jobs_rx: Mutex<mpsc::Receiver<ScanJob>>,
    /// Async job statuses by job id. A std mutex: never held across an
    /// await, so queuing a job and recording its status is one step.
    job_status: StdMutex<HashMap<Uuid, JobEntry>>,
    metrics: Arc<dyn EvidenceMetricsPort>,
    evidence_tx: mpsc::Sender<PendingWrite>,
    /// Writes handed to the writer and not yet finished.
    writes: TaskTracker,
}

/// The single task that inserts flagged scans' rows, one scan per
/// transaction. Created with its [`Service`]; the gear runs it.
pub struct EvidenceWriter<SR>
where
    SR: EvidenceRepository + 'static,
{
    db: Arc<DbProvider>,
    repo: Arc<SR>,
    metrics: Arc<dyn EvidenceMetricsPort>,
    rx: mpsc::Receiver<PendingWrite>,
    write_timeout: Duration,
}

impl<SR> EvidenceWriter<SR>
where
    SR: EvidenceRepository + 'static,
{
    /// Write queued scans until `cancel` fires or every sender is gone.
    /// Scans still queued at cancellation are dropped and counted.
    pub async fn run(mut self, cancel: CancellationToken) {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                next = self.rx.recv() => match next {
                    Some(write) => self.write(write).await,
                    None => return,
                },
            }
        }
        self.rx.close();
        let mut dropped = 0_u64;
        while let Ok(write) = self.rx.try_recv() {
            dropped += 1;
            self.metrics.evidence_write_failed();
            drop(write);
        }
        if dropped > 0 {
            tracing::warn!(
                scans = dropped,
                "shutdown: flagged scans still queued for writing were dropped; their evidence is lost"
            );
        }
    }

    /// Insert one scan's rows in one transaction, within `write_timeout`. A
    /// failure is logged without content and counted.
    async fn write(&self, write: PendingWrite) {
        let PendingWrite { scope, rows, .. } = write;
        let count = rows.len();
        let Some(trace_id) = rows.first().map(|r| r.trace_id) else {
            return;
        };
        let repo = Arc::clone(&self.repo);
        let insert = self.db.transaction(|tx| {
            Box::pin(async move {
                repo.insert_all(tx, &scope, rows)
                    .await
                    .map_err(|e| toolkit_db::DbError::Other(anyhow::Error::new(e)))
            })
        });
        let failure = match tokio::time::timeout(self.write_timeout, insert).await {
            Ok(Ok(())) => return,
            Ok(Err(e)) => e.to_string(),
            Err(_elapsed) => format!("timed out after {:?}", self.write_timeout),
        };
        self.metrics.evidence_write_failed();
        tracing::error!(
            trace_id = %trace_id,
            rows = count,
            error = %failure,
            "failed to store a flagged scan's findings; its evidence is lost"
        );
    }
}

impl<SR> Service<SR>
where
    SR: EvidenceRepository + 'static,
{
    /// The service and its evidence writer, which the caller must run (see
    /// [`EvidenceWriter::run`]): until it does, flagged scans queue up to
    /// `evidence.write_queue_capacity` and are then dropped.
    #[must_use]
    pub fn new(
        db: Arc<DbProvider>,
        hub: Arc<ClientHub>,
        repo: Arc<SR>,
        default_policy_provider: Option<Arc<dyn PolicyProvider>>,
        policy_enforcer: PolicyEnforcer,
        metrics: Arc<dyn EvidenceMetricsPort>,
        settings: ServiceSettings,
    ) -> (Self, EvidenceWriter<SR>) {
        let (jobs_tx, jobs_rx) = mpsc::channel(settings.job_queue_capacity.max(1));
        let (evidence_tx, evidence_rx) =
            mpsc::channel(settings.evidence.write_queue_capacity.max(1));
        let writer = EvidenceWriter {
            db: Arc::clone(&db),
            repo: Arc::clone(&repo),
            metrics: Arc::clone(&metrics),
            rx: evidence_rx,
            write_timeout: settings.evidence.write_timeout,
        };
        let service = Self {
            db,
            hub,
            repo,
            default_policy_provider,
            policy_enforcer,
            settings,
            jobs_tx,
            jobs_rx: Mutex::new(jobs_rx),
            job_status: StdMutex::new(HashMap::new()),
            metrics,
            evidence_tx,
            writes: TaskTracker::new(),
        };
        (service, writer)
    }

    // ── Authorization ────────────────────────────────────────────────

    /// The tenant every call runs for: the authenticated caller's own, never
    /// one named in the request. A nil tenant (a tenantless service token) is
    /// not a tenant and is refused.
    fn caller_tenant(ctx: &SecurityContext) -> Result<Uuid, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        if tenant_id.is_nil() {
            tracing::warn!(
                subject_id = %ctx.subject_id(),
                "request refused: caller's token carries no tenant"
            );
            return Err(DomainError::Forbidden);
        }
        Ok(tenant_id)
    }

    /// Ask the PDP, logging a refusal with what was asked.
    async fn decide(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &'static str,
        resource_id: Option<Uuid>,
        tenant_id: Uuid,
        require_constraints: bool,
    ) -> Result<AccessScope, DomainError> {
        let request = AccessRequest::new()
            .resource_property(pep_properties::OWNER_TENANT_ID, tenant_id)
            .require_constraints(require_constraints);
        self.policy_enforcer
            .access_scope_with(ctx, resource, action, resource_id, &request)
            .await
            .map_err(|e| {
                if matches!(
                    e,
                    EnforcerError::AccessDenied(_) | EnforcerError::CompileFailed(_)
                ) {
                    tracing::warn!(
                        resource = resource.name(),
                        action,
                        tenant_id = %tenant_id,
                        subject_id = %ctx.subject_id(),
                        error = %e,
                        "authorization denied"
                    );
                } else {
                    // `EvaluationFailed`, or an outcome class added later.
                    tracing::error!(
                        resource = resource.name(),
                        action,
                        tenant_id = %tenant_id,
                        error = %e,
                        "authorization could not be evaluated"
                    );
                }
                DomainError::from(e)
            })
    }

    /// The PDP scope for `action` on the caller's scan records, for a
    /// Secure ORM query. An unconstrained decision is refused rather than
    /// narrowed: the gateway never reads or writes rows without a
    /// PDP-given tenant constraint.
    async fn authorize_records(
        &self,
        ctx: &SecurityContext,
        action: &'static str,
        resource_id: Option<Uuid>,
    ) -> Result<(Uuid, AccessScope), DomainError> {
        let tenant_id = Self::caller_tenant(ctx)?;
        let scope = self
            .decide(
                ctx,
                &resources::SCAN_RECORD,
                action,
                resource_id,
                tenant_id,
                true,
            )
            .await?;
        if scope.is_unconstrained() {
            tracing::warn!(
                action,
                tenant_id = %tenant_id,
                "authorization refused: the PDP returned no constraints for a row access"
            );
            return Err(DomainError::Forbidden);
        }
        Ok((tenant_id, scope))
    }

    /// A yes/no PDP decision on a resource that has no rows (a policy, the
    /// backend list, an in-memory job): allowed when the scope is
    /// unconstrained or names the caller's tenant.
    async fn authorize_tenant_resource(
        &self,
        ctx: &SecurityContext,
        resource: &ResourceType,
        action: &'static str,
    ) -> Result<Uuid, DomainError> {
        let tenant_id = Self::caller_tenant(ctx)?;
        let scope = self
            .decide(ctx, resource, action, None, tenant_id, false)
            .await?;
        if scope.is_unconstrained()
            || scope
                .all_uuid_values_for(pep_properties::OWNER_TENANT_ID)
                .contains(&tenant_id)
        {
            return Ok(tenant_id);
        }
        tracing::warn!(
            resource = resource.name(),
            action,
            tenant_id = %tenant_id,
            "authorization denied: the PDP scope does not include the caller's tenant"
        );
        Err(DomainError::Forbidden)
    }

    // ── Plugin discovery ─────────────────────────────────────────────

    /// Every registered instance of the plugin type `type_id`.
    async fn registered_plugins(
        &self,
        type_id: String,
    ) -> Result<Vec<RegisteredPlugin>, DomainError> {
        let registry = self.hub.get::<dyn TypesRegistryClient>()?;
        let instances = registry
            .list_instances(InstanceQuery::new().with_pattern(format!("{type_id}*")))
            .await?;
        // The pattern is the registry's filter; check the prefix here too, so
        // a registry that matches loosely cannot hand one plugin type's
        // instance to another's resolution.
        Ok(instances
            .into_iter()
            .filter(|inst| inst.id.as_ref().starts_with(type_id.as_str()))
            .map(|inst| RegisteredPlugin {
                gts_id: inst.id.to_string(),
                header: serde_json::from_value(inst.object),
            })
            .collect())
    }

    /// The policy-resolution failure `reason`: an error when `fail_mode` is
    /// closed, a warning when open.
    fn policy_resolution_failed(&self, tenant_id: Uuid, reason: String) -> Result<(), DomainError> {
        match self.settings.fail_mode {
            FailMode::Closed => {
                tracing::warn!(tenant_id = %tenant_id, reason = %reason, "tenant policy unavailable (fail_mode = closed)");
                Err(DomainError::PolicyUnavailable { tenant_id, reason })
            }
            FailMode::Open => {
                tracing::warn!(tenant_id = %tenant_id, reason = %reason, "tenant policy unresolved; using the fallback (fail_mode = open)");
                Ok(())
            }
        }
    }

    /// Resolve the currently authoritative [`PolicyProvider`]: the
    /// highest-priority registered `PolicyProviderSpecV1` plugin instance,
    /// or the config-backed default if none is registered. `None` only under
    /// `fail_mode: open`, when there is nothing to ask: the caller uses the
    /// permissive default. See `prompt_scanner_sdk::PolicyProvider`'s docs
    /// for why this is a single-instance pick.
    async fn resolve_policy_provider(
        &self,
        tenant_id: Uuid,
    ) -> Result<Option<Arc<dyn PolicyProvider>>, DomainError> {
        let plugins = self
            .registered_plugins(PolicyProviderSpecV1::gts_type_id().to_string())
            .await?;

        let mut best: Option<(String, i16)> = None;
        for plugin in plugins {
            match plugin.header {
                Ok(header) => {
                    if best.as_ref().is_none_or(|(_, p)| header.priority < *p) {
                        best = Some((plugin.gts_id, header.priority));
                    }
                }
                Err(e) => self.policy_resolution_failed(
                    tenant_id,
                    format!(
                        "malformed policy provider plugin instance {}: {e}",
                        plugin.gts_id
                    ),
                )?,
            }
        }

        if let Some((gts_id, _)) = best {
            if let Some(provider) = self
                .hub
                .try_get_scoped::<dyn PolicyProvider>(&ClientScope::gts_id(&gts_id))
            {
                return Ok(Some(provider));
            }
            self.policy_resolution_failed(
                tenant_id,
                format!("policy provider plugin {gts_id} is registered but has no client"),
            )?;
        }

        if let Some(provider) = &self.default_policy_provider {
            return Ok(Some(Arc::clone(provider)));
        }
        self.policy_resolution_failed(
            tenant_id,
            "no policy provider plugin is registered and no fallback_policy is configured"
                .to_owned(),
        )?;
        Ok(None)
    }

    // ── Fan-out backend resolution ───────────────────────────────────

    /// Resolve every registered `ScannerBackendSpecV1` instance whose
    /// `vendor` is present in `enabled`.
    ///
    /// Instances that fail to deserialize, or whose scoped `ClientHub` entry
    /// isn't registered yet, are skipped with a warning; `run_backends`
    /// fails the scan when a *required* backend is among them.
    async fn resolve_backends(
        &self,
        enabled: &[String],
    ) -> Result<Vec<ResolvedBackend>, DomainError> {
        if enabled.is_empty() {
            return Ok(Vec::new());
        }

        let mut resolved = Vec::new();
        for plugin in self
            .registered_plugins(ScannerBackendSpecV1::gts_type_id().to_string())
            .await?
        {
            let header = match plugin.header {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(gts_id = %plugin.gts_id, error = %e, "malformed scanner backend plugin instance; skipped");
                    continue;
                }
            };
            if !enabled.contains(&header.vendor) {
                continue;
            }
            let scope = ClientScope::gts_id(&plugin.gts_id);
            match self.hub.try_get_scoped::<dyn ScannerBackendClient>(&scope) {
                Some(client) => resolved.push(ResolvedBackend {
                    vendor: header.vendor,
                    gts_id: plugin.gts_id,
                    client,
                }),
                None => {
                    tracing::warn!(vendor = %header.vendor, gts_id = %plugin.gts_id, "scanner backend client not registered yet");
                }
            }
        }
        Ok(resolved)
    }

    /// Stamp the backend's identity on its detections: `detector` is its GTS
    /// instance id, `detector_version` the version it reports.
    fn stamp(backend: &ResolvedBackend, detections: Vec<BackendDetection>) -> Vec<Finding> {
        let version = backend.client.version();
        detections
            .into_iter()
            .map(|d| Finding::from_detection(d, backend.gts_id.clone(), version.clone()))
            .collect()
    }

    /// Fan out to every resolved backend concurrently (with a per-backend
    /// timeout), with the input in [`ScanPhase::Inline`].
    ///
    /// A required backend (per `policy.required_backends`) that is
    /// unresolvable fails the call with `BackendUnavailable`, one that times
    /// out with `BackendTimeout`, one that errors with
    /// `RequiredBackendFailed`. A non-required backend in the same situation
    /// is left out of the findings and listed in `unscored` (degraded
    /// coverage): logged, counted, never a change to the verdict action. A
    /// backend that returns [`BackendOutcome::Defer`] (required or not) is
    /// returned for background processing and listed in `unscored` as
    /// `deferred`.
    #[expect(
        clippy::cognitive_complexity,
        reason = "one match over the outcomes; the tracing macros add most of the score"
    )]
    async fn run_backends(
        &self,
        ctx: &SecurityContext,
        input: &ScanInput,
        policy: &TenantPolicy,
        trace_id: Uuid,
    ) -> Result<BackendRun, DomainError> {
        let resolved = self.resolve_backends(&policy.enabled_backends).await?;
        let mut run = BackendRun::empty();

        for vendor in &policy.enabled_backends {
            if resolved.iter().any(|b| &b.vendor == vendor) {
                continue;
            }
            if policy.required_backends.contains(vendor) {
                tracing::warn!(backend = %vendor, trace_id = %trace_id, "required backend has no resolvable plugin instance");
                return Err(DomainError::BackendUnavailable {
                    backend: vendor.clone(),
                    reason: "no resolvable plugin instance for this vendor".to_owned(),
                });
            }
            tracing::warn!(backend = %vendor, trace_id = %trace_id, "enabled backend has no resolvable plugin instance; scan coverage degraded");
            run.unscored_push(vendor, UnscoredReason::Unavailable);
        }
        // A required backend the policy forgot to enable never runs.
        for required in &policy.required_backends {
            if !resolved.iter().any(|b| &b.vendor == required)
                && !policy.enabled_backends.contains(required)
            {
                tracing::warn!(backend = %required, trace_id = %trace_id, "required backend has no resolvable plugin instance");
                return Err(DomainError::BackendUnavailable {
                    backend: required.clone(),
                    reason: "no resolvable plugin instance for this vendor".to_owned(),
                });
            }
        }

        let timeout = self.settings.backend_timeout;
        let futs = resolved.into_iter().map(|backend| async move {
            let outcome = tokio::time::timeout(timeout, backend.client.detect(ctx, input)).await;
            (backend, outcome)
        });
        let outcomes = futures::future::join_all(futs).await;

        run.findings.reserve(outcomes.len());
        for (backend, outcome) in outcomes {
            let required = policy.required_backends.contains(&backend.vendor);
            match outcome {
                Ok(Ok(BackendOutcome::Detections(detections))) => {
                    run.findings.extend(Self::stamp(&backend, detections));
                }
                Ok(Ok(BackendOutcome::Defer)) => {
                    tracing::info!(backend = %backend.vendor, trace_id = %trace_id, "backend deferred; queuing for background processing");
                    // Listed even when required: its findings are not in
                    // this verdict.
                    run.unscored_push(&backend.vendor, UnscoredReason::Deferred);
                    run.deferred.push(backend);
                }
                Ok(Err(e)) => {
                    tracing::warn!(backend = %backend.vendor, trace_id = %trace_id, required, error = %e, "backend returned an error");
                    if required {
                        return Err(DomainError::RequiredBackendFailed {
                            backend: backend.vendor,
                            reason: e.to_string(),
                        });
                    }
                    run.unscored_push(&backend.vendor, Self::unscored_reason(&e));
                }
                Err(_elapsed) => {
                    tracing::warn!(backend = %backend.vendor, trace_id = %trace_id, required, "backend timed out");
                    if required {
                        return Err(DomainError::BackendTimeout {
                            backend: backend.vendor,
                        });
                    }
                    run.unscored_push(&backend.vendor, UnscoredReason::Timeout);
                }
            }
        }

        for unscored in &run.unscored {
            self.metrics.backend_unscored(unscored.reason);
        }
        if !run.unscored.is_empty() {
            let backends: Vec<_> = run
                .unscored
                .iter()
                .map(|u| format!("{}:{}", u.backend, u.reason.as_str()))
                .collect();
            tracing::warn!(trace_id = %trace_id, unscored = ?backends, "scan coverage degraded: some enabled backends did not contribute to the verdict");
        }

        Ok(run)
    }

    /// The stable reason a backend's own error is reported under.
    fn unscored_reason(e: &PromptScannerError) -> UnscoredReason {
        match e {
            PromptScannerError::BackendUnavailable { .. } => UnscoredReason::Unavailable,
            PromptScannerError::BackendTimeout { .. } => UnscoredReason::Timeout,
            _ => UnscoredReason::Error,
        }
    }

    /// List every registered scanner backend plugin instance, regardless of
    /// tenant policy — used by `GET /prompt-scanner/v1/backends` for
    /// operator visibility. Needs `list` on the backend resource.
    #[instrument(skip(self, ctx))]
    pub async fn list_backend_instances(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<BackendInstanceInfo>, DomainError> {
        self.authorize_tenant_resource(ctx, &resources::BACKEND, actions::LIST)
            .await?;
        let plugins = self
            .registered_plugins(ScannerBackendSpecV1::gts_type_id().to_string())
            .await?;
        let mut out = Vec::with_capacity(plugins.len());
        for plugin in plugins {
            let header = match plugin.header {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(gts_id = %plugin.gts_id, error = %e, "malformed scanner backend plugin instance; skipped");
                    continue;
                }
            };
            let available = self
                .hub
                .try_get_scoped::<dyn ScannerBackendClient>(&ClientScope::gts_id(&plugin.gts_id))
                .is_some();
            out.push(BackendInstanceInfo {
                gts_id: plugin.gts_id,
                vendor: header.vendor,
                priority: header.priority,
                available,
            });
        }
        Ok(out)
    }

    // ── Business logic ───────────────────────────────────────────────

    /// Turn the request's content into the text to scan: a
    /// [`ScanContent::File`] (only valid with `ContentKind::Upload`, checked
    /// by [`ScanRequest::validate`]) is replaced by the text `file-parser`
    /// extracts from it, within `attachment_parse_timeout`, so the rest of
    /// the pipeline only ever deals with text. The extracted text obeys the
    /// same size limit as a text payload.
    async fn resolve_effective_payload(
        &self,
        ctx: &SecurityContext,
        content: ScanContent,
    ) -> Result<String, DomainError> {
        let attachment = match content {
            ScanContent::Text(text) => return Ok(text),
            ScanContent::File(attachment) => attachment,
        };

        let client = self.hub.get::<dyn FileParserClientV1>()?;
        let request = ParseBytesRequest {
            filename: attachment.filename,
            content_type: attachment.mime_type,
            bytes: attachment.data,
            // A connector's declared MIME type is an untrusted hint — let
            // content-based detection correct a wrong or missing one.
            detection: Detection::Auto,
        };
        let parsed = tokio::time::timeout(
            self.settings.attachment_parse_timeout,
            client.parse_bytes(ctx, request),
        )
        .await
        .map_err(|_elapsed| {
            tracing::warn!(timeout = ?self.settings.attachment_parse_timeout, "attachment parsing timed out");
            DomainError::BackendTimeout {
                backend: FILE_PARSER.to_owned(),
            }
        })??;

        if parsed.markdown.is_empty() {
            return Err(DomainError::validation(
                "attachment",
                "the attachment holds no text to scan",
            ));
        }
        if parsed.markdown.len() > MAX_PAYLOAD_BYTES {
            return Err(DomainError::validation(
                "attachment",
                format!("the attachment's text exceeds {MAX_PAYLOAD_BYTES} bytes"),
            ));
        }
        Ok(parsed.markdown)
    }

    fn validate_request(req: &ScanRequest) -> Result<(), DomainError> {
        req.validate().map_err(invalid_request)
    }

    fn validate_correlation_id(correlation_id: &str) -> Result<(), DomainError> {
        prompt_scanner_sdk::validate_correlation_id(correlation_id).map_err(invalid_request)
    }

    /// Check a policy against the schema and the per-scan limits.
    pub(crate) fn validate_policy(policy: &TenantPolicy) -> Result<(), DomainError> {
        use policy_limits::{
            MAX_BACKENDS, MAX_ENTRIES, MAX_NAME_CHARS, MAX_RETENTION, MAX_SETTINGS_SCHEMA_CHARS,
        };
        let name_ok = |s: &str| !s.trim().is_empty() && s.chars().count() <= MAX_NAME_CHARS;

        for (field, list) in [
            ("enabled_backends", &policy.enabled_backends),
            ("required_backends", &policy.required_backends),
        ] {
            if list.len() > MAX_BACKENDS {
                return Err(DomainError::validation(
                    field,
                    format!("at most {MAX_BACKENDS} entries"),
                ));
            }
            if let Some(bad) = list.iter().find(|v| !name_ok(v)) {
                return Err(DomainError::validation(
                    field,
                    format!(
                        "'{}' must be 1..={MAX_NAME_CHARS} characters",
                        truncate(bad)
                    ),
                ));
            }
        }
        if policy.thresholds.len() > MAX_ENTRIES {
            return Err(DomainError::validation(
                "thresholds",
                format!("at most {MAX_ENTRIES} entries"),
            ));
        }
        for (category, threshold) in &policy.thresholds {
            if !name_ok(category) {
                return Err(DomainError::validation(
                    "thresholds",
                    format!("category keys must be 1..={MAX_NAME_CHARS} characters"),
                ));
            }
            if !(0.0..=1.0).contains(threshold) {
                return Err(DomainError::validation(
                    "thresholds",
                    format!("threshold for '{category}' must be in [0.0, 1.0]"),
                ));
            }
        }
        if policy.category_action_overrides.len() > MAX_ENTRIES {
            return Err(DomainError::validation(
                "category_action_overrides",
                format!("at most {MAX_ENTRIES} entries"),
            ));
        }
        if policy.category_action_overrides.keys().any(|k| !name_ok(k)) {
            return Err(DomainError::validation(
                "category_action_overrides",
                format!("category keys must be 1..={MAX_NAME_CHARS} characters"),
            ));
        }
        if policy.content_kinds.len() > prompt_scanner_sdk::ContentKind::ALL.len() {
            return Err(DomainError::validation(
                "content_kinds",
                "lists a content kind more than once",
            ));
        }
        if policy
            .evidence
            .actions
            .as_ref()
            .is_some_and(|a| a.len() > MAX_ENTRIES)
        {
            return Err(DomainError::validation(
                "evidence.actions",
                format!("at most {MAX_ENTRIES} entries"),
            ));
        }
        for required in &policy.required_backends {
            if !policy.enabled_backends.contains(required) {
                return Err(DomainError::validation(
                    "required_backends",
                    format!(
                        "'{}' must also be listed in enabled_backends",
                        truncate(required)
                    ),
                ));
            }
        }
        if let Some(retention) = policy.retention
            && (retention.is_zero() || retention > MAX_RETENTION)
        {
            return Err(DomainError::validation(
                "retention",
                "retention must be greater than zero and at most 100 years",
            ));
        }
        if let Some(schema) = &policy.settings_schema
            && (schema.is_empty() || schema.chars().count() > MAX_SETTINGS_SCHEMA_CHARS)
        {
            return Err(DomainError::validation(
                "settings_schema",
                format!("must be 1..={MAX_SETTINGS_SCHEMA_CHARS} characters"),
            ));
        }
        Ok(())
    }

    /// Synchronous scan: run backends, aggregate, return the verdict. A
    /// flagged scan's findings are stored after it, in the background;
    /// backends that deferred are queued.
    #[instrument(skip(self, ctx, req), fields(tenant_id = %ctx.subject_tenant_id(), connector_id = %req.connector_id))]
    pub async fn scan(
        &self,
        ctx: &SecurityContext,
        req: ScanRequest,
    ) -> Result<ScanVerdict, DomainError> {
        let (verdict, deferred) = self.scan_inner(ctx, req, Uuid::now_v7()).await?;
        if let Some(deferred) = deferred {
            self.queue_deferred(ctx, deferred);
        }
        Ok(verdict)
    }

    /// Queue a synchronous scan's deferred backends. A full queue drops a
    /// backend's contribution, counted and logged.
    fn queue_deferred(&self, ctx: &SecurityContext, deferred: Deferred) {
        for backend in deferred.backends {
            let vendor = backend.vendor.clone();
            let required = deferred.scan.policy.required_backends.contains(&vendor);
            let job = ScanJob::DeferredBackend {
                ctx: ctx.clone(),
                backend,
                scan: Arc::clone(&deferred.scan),
            };
            if self.jobs_tx.try_send(job).is_err() {
                self.metrics.jobs_dropped(1);
                self.metrics.deferred_backend_failed(required);
                tracing::warn!(
                    trace_id = %deferred.scan.facts.trace_id,
                    backend = %vendor,
                    required,
                    "job queue full; a deferred backend's contribution is dropped"
                );
            }
        }
    }

    /// The scan itself; deferred backends are returned, not queued.
    async fn scan_inner(
        &self,
        ctx: &SecurityContext,
        req: ScanRequest,
        trace_id: Uuid,
    ) -> Result<(ScanVerdict, Option<Deferred>), DomainError> {
        Self::caller_tenant(ctx)?;
        Self::validate_request(&req)?;
        // Authorize before the content is parsed or sent anywhere.
        let (tenant_id, scope) = self.authorize_records(ctx, actions::CREATE, None).await?;
        let ScanRequest {
            connector_id,
            correlation_id,
            content_kind,
            content,
            metadata,
        } = req;
        let from_file = matches!(content, ScanContent::File(_));
        let payload = self.resolve_effective_payload(ctx, content).await?;

        let policy = self.load_policy(ctx, tenant_id).await?;
        let input = ScanInput::new(tenant_id, content_kind, payload).with_metadata(metadata);
        let governed = policy.content_kinds.contains(&content_kind);
        let BackendRun {
            findings,
            deferred,
            unscored,
        } = if governed {
            self.run_backends(ctx, &input, &policy, trace_id).await?
        } else {
            BackendRun::empty()
        };
        let (action, considered) = if governed {
            aggregate_verdict(&findings, &policy)
        } else {
            // Not this policy's business: no backends, allowed, nothing stored.
            tracing::debug!(content_kind = ?content_kind, "content kind not governed by the tenant policy");
            (VerdictAction::Allow, Vec::new())
        };

        let redacted_payload = (action == VerdictAction::Redact)
            .then(|| prompt_scanner_sdk::redact_spans(&input.payload, &considered));

        let facts = ScanFacts {
            trace_id,
            tenant_id,
            correlation_id,
            connector_id,
            content_kind,
            created_at: Utc::now(),
            settings_schema: policy.settings_schema.clone(),
            retention: policy.retention,
            evidence: policy.evidence.clone(),
            from_file,
        };

        let built = build_rows(
            &facts,
            action,
            &considered,
            &input.payload,
            0,
            &self.settings.evidence,
        );
        let next_ordinal = u16::try_from(built.rows.len()).unwrap_or(u16::MAX);
        self.metrics.scan_completed(!considered.is_empty());
        self.store(&scope, built);

        let created_at = facts.created_at;
        let deferred = (!deferred.is_empty()).then(|| Deferred {
            backends: deferred,
            scan: Arc::new(DeferredScan {
                // The background retry tells the backend not to defer again.
                input: input.with_phase(ScanPhase::Deferred),
                facts,
                policy,
                scope,
                state: Mutex::new(DeferredState {
                    considered: considered.clone(),
                    next_ordinal,
                }),
            }),
        });

        let verdict = ScanVerdict {
            trace_id,
            tenant_id,
            action,
            findings: considered,
            redacted_payload,
            unscored_backends: unscored,
            created_at,
        };
        Ok((verdict, deferred))
    }

    /// Hand a scan's rows to the evidence writer, counting the findings left
    /// out. A full queue drops the rows, counted and logged.
    fn store(&self, scope: &AccessScope, built: BuiltRows) {
        self.count_not_stored(&built);
        let rows = built.rows;
        let Some(trace_id) = rows.first().map(|r| r.trace_id) else {
            return;
        };
        let write = PendingWrite {
            scope: scope.clone(),
            rows,
            _token: self.writes.token(),
        };
        match self.evidence_tx.try_send(write) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(write)) => {
                self.metrics
                    .findings_not_stored(NotStored::QueueFull, write.rows.len() as u64);
                tracing::warn!(trace_id = %trace_id, "evidence write queue full; the scan's findings are not stored");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.metrics.evidence_write_failed();
                tracing::warn!(trace_id = %trace_id, "evidence writer stopped; the scan's findings are not stored");
            }
        }
    }

    fn count_not_stored(&self, built: &BuiltRows) {
        self.metrics
            .findings_not_stored(NotStored::OverCap, built.over_cap);
        self.metrics
            .findings_not_stored(NotStored::InvalidValue, built.invalid);
        if built.invalid > 0 {
            tracing::warn!(
                count = built.invalid,
                "findings with values the schema cannot hold were not stored; a scanner plugin defect"
            );
        }
    }

    /// Wait until every write handed to the writer so far has finished. For
    /// shutdown and tests; needs the writer running.
    pub async fn wait_for_pending_writes(&self) {
        self.writes.close();
        self.writes.wait().await;
        self.writes.reopen();
    }

    // ── Async jobs ───────────────────────────────────────────────────

    fn job_table(&self) -> MutexGuard<'_, HashMap<Uuid, JobEntry>> {
        self.job_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn set_job_status(&self, job_id: Uuid, tenant_id: Uuid, status: ScanJobStatus) {
        let finished_at = matches!(
            status,
            ScanJobStatus::Completed { .. } | ScanJobStatus::Failed { .. }
        )
        .then(Instant::now);
        self.job_table().insert(
            job_id,
            JobEntry {
                tenant_id,
                status,
                finished_at,
            },
        );
    }

    /// Forget finished jobs older than `job_status_ttl`.
    fn evict_finished_jobs(&self) {
        let ttl = self.settings.job_status_ttl;
        let now = Instant::now();
        self.job_table()
            .retain(|_, e| e.finished_at.is_none_or(|at| now.duration_since(at) < ttl));
    }

    /// Queue a scan for asynchronous processing, after authorizing it. The
    /// worker ([`Self::drain_jobs`]) runs it under the returned `trace_id`.
    ///
    /// Cancel-safe: the job is queued and its status recorded in one step,
    /// with no await in between.
    ///
    /// # Errors
    ///
    /// `Overloaded` when the queue is full; the usual validation and
    /// authorization errors.
    #[instrument(skip(self, ctx, req), fields(tenant_id = %ctx.subject_tenant_id()))]
    pub async fn scan_async(
        &self,
        ctx: &SecurityContext,
        req: ScanRequest,
    ) -> Result<ScanJobHandle, DomainError> {
        Self::caller_tenant(ctx)?;
        Self::validate_request(&req)?;
        let (tenant_id, _scope) = self.authorize_records(ctx, actions::CREATE, None).await?;

        let job_id = Uuid::now_v7();
        let trace_id = Uuid::now_v7();
        let job = ScanJob::Scan {
            job_id,
            trace_id,
            ctx: ctx.clone(),
            request: Box::new(req),
        };

        let mut table = self.job_table();
        self.jobs_tx.try_send(job).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                self.metrics.jobs_dropped(1);
                tracing::warn!("async job queue full; scan_async refused");
                DomainError::Overloaded
            }
            mpsc::error::TrySendError::Closed(_) => {
                DomainError::Internal("async job queue is closed".to_owned())
            }
        })?;
        table.insert(
            job_id,
            JobEntry {
                tenant_id,
                status: ScanJobStatus::Queued,
                finished_at: None,
            },
        );
        drop(table);

        Ok(ScanJobHandle {
            job_id,
            trace_id,
            status: ScanJobStatus::Queued,
        })
    }

    /// Status of an async job queued by the caller's own tenant. Another
    /// tenant's job, and a job forgotten after `job_status_ttl`, read as
    /// `JobNotFound`.
    #[instrument(skip(self, ctx))]
    pub async fn get_job_status(
        &self,
        ctx: &SecurityContext,
        job_id: Uuid,
    ) -> Result<ScanJobStatus, DomainError> {
        let tenant_id = self
            .authorize_tenant_resource(ctx, &resources::SCAN_RECORD, actions::GET)
            .await?;
        match self.job_table().get(&job_id) {
            Some(entry) if entry.tenant_id == tenant_id => Ok(entry.status),
            _ => Err(DomainError::job_not_found(job_id)),
        }
    }

    /// Run at most `max` queued jobs, stopping early when `cancel` fires —
    /// also in the middle of a job, which then fails. Forgets expired job
    /// statuses first. Returns the jobs taken off the queue.
    pub async fn drain_jobs(&self, max: usize, cancel: &CancellationToken) -> usize {
        self.evict_finished_jobs();
        let mut rx = self.jobs_rx.lock().await;
        let mut taken = 0;
        while taken < max && !cancel.is_cancelled() {
            let Ok(job) = rx.try_recv() else {
                break;
            };
            taken += 1;
            self.run_job(job, cancel).await;
        }
        taken
    }

    /// Run the next queued job, if any. Returns whether there was one.
    pub async fn process_next_job(&self) -> bool {
        self.drain_jobs(1, &CancellationToken::new()).await > 0
    }

    /// Shutdown: close the queue and drop what is still in it, failing the
    /// `scan_async` jobs among them. Returns the jobs dropped.
    pub async fn drop_queued_jobs(&self) -> usize {
        let mut rx = self.jobs_rx.lock().await;
        rx.close();
        let mut dropped = 0_usize;
        while let Ok(job) = rx.try_recv() {
            dropped += 1;
            if let ScanJob::Scan { job_id, ctx, .. } = job {
                self.set_job_status(
                    job_id,
                    ctx.subject_tenant_id(),
                    ScanJobStatus::Failed {
                        code: ScanJobFailureCode::Internal,
                    },
                );
            }
        }
        if dropped > 0 {
            self.metrics.jobs_dropped(dropped as u64);
            tracing::warn!(
                jobs = dropped,
                "shutdown: queued async jobs were dropped before they ran"
            );
        }
        dropped
    }

    async fn run_job(&self, job: ScanJob, cancel: &CancellationToken) {
        match job {
            ScanJob::Scan {
                job_id,
                trace_id,
                ctx,
                request,
            } => {
                self.run_async_scan(job_id, trace_id, &ctx, *request, cancel)
                    .await;
            }
            ScanJob::DeferredBackend { ctx, backend, scan } => {
                self.run_deferred_job(&ctx, &backend, &scan, cancel).await;
            }
        }
    }

    /// Run a `scan_async` job, recording its status as it goes.
    async fn run_async_scan(
        &self,
        job_id: Uuid,
        trace_id: Uuid,
        ctx: &SecurityContext,
        request: ScanRequest,
        cancel: &CancellationToken,
    ) {
        let tenant_id = ctx.subject_tenant_id();
        self.set_job_status(job_id, tenant_id, ScanJobStatus::Running);
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            r = self.run_scan_job(ctx, request, trace_id) => Some(r),
        };
        let status = match result {
            Some(Ok(())) => ScanJobStatus::Completed { trace_id },
            Some(Err(e)) => Self::failed_job(job_id, e),
            None => {
                self.metrics.jobs_dropped(1);
                tracing::warn!(job_id = %job_id, "async scan job interrupted by shutdown");
                ScanJobStatus::Failed {
                    code: ScanJobFailureCode::Internal,
                }
            }
        };
        self.set_job_status(job_id, tenant_id, status);
    }

    fn failed_job(job_id: Uuid, e: DomainError) -> ScanJobStatus {
        if e.is_caller_fault() {
            tracing::info!(job_id = %job_id, error = %e, "async scan job refused");
        } else {
            tracing::warn!(job_id = %job_id, error = %e, "async scan job failed");
        }
        ScanJobStatus::Failed {
            code: PromptScannerError::from(e).job_failure_code(),
        }
    }

    /// Run a synchronous scan's deferred backend.
    async fn run_deferred_job(
        &self,
        ctx: &SecurityContext,
        backend: &ResolvedBackend,
        scan: &DeferredScan,
        cancel: &CancellationToken,
    ) {
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => None,
            r = self.process_deferred_backend(ctx, backend, scan) => Some(r),
        };
        match result {
            Some(Ok(())) => {}
            Some(Err(e)) => self.report_deferred_failure(scan, backend, &e),
            None => {
                self.metrics.jobs_dropped(1);
                self.report_deferred_failure(
                    scan,
                    backend,
                    &DomainError::Internal("interrupted by shutdown".to_owned()),
                );
            }
        }
    }

    /// A `scan_async` job: the scan, then its deferred backends right away
    /// (the caller is polling anyway). A required one that fails, times out
    /// or defers again fails the job.
    async fn run_scan_job(
        &self,
        ctx: &SecurityContext,
        request: ScanRequest,
        trace_id: Uuid,
    ) -> Result<(), DomainError> {
        let (_verdict, deferred) = self.scan_inner(ctx, request, trace_id).await?;
        let Some(deferred) = deferred else {
            return Ok(());
        };
        let scan = &deferred.scan;
        let results = futures::future::join_all(
            deferred
                .backends
                .iter()
                .map(|b| async move { (b, self.process_deferred_backend(ctx, b, scan).await) }),
        )
        .await;
        let mut failure = None;
        for (backend, result) in results {
            if let Err(e) = result {
                self.report_deferred_failure(scan, backend, &e);
                if scan.policy.required_backends.contains(&backend.vendor) && failure.is_none() {
                    failure = Some(e);
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn report_deferred_failure(
        &self,
        scan: &DeferredScan,
        backend: &ResolvedBackend,
        e: &DomainError,
    ) {
        let required = scan.policy.required_backends.contains(&backend.vendor);
        self.metrics.deferred_backend_failed(required);
        tracing::warn!(
            trace_id = %scan.facts.trace_id,
            backend = %backend.vendor,
            required,
            error = %e,
            "deferred backend produced no result; its findings are missing from the scan"
        );
    }

    /// Call a previously-deferred backend (within `deferred_backend_timeout`)
    /// and append its findings to the scan as further rows.
    ///
    /// # Errors
    ///
    /// `BackendTimeout` on timeout, `RequiredBackendFailed` when the backend
    /// errors or defers again.
    async fn process_deferred_backend(
        &self,
        ctx: &SecurityContext,
        backend: &ResolvedBackend,
        scan: &DeferredScan,
    ) -> Result<(), DomainError> {
        let outcome = tokio::time::timeout(
            self.settings.deferred_backend_timeout,
            backend.client.detect(ctx, &scan.input),
        )
        .await;
        let detections = match outcome {
            Ok(Ok(BackendOutcome::Detections(detections))) => detections,
            Ok(Ok(BackendOutcome::Defer)) => {
                return Err(DomainError::RequiredBackendFailed {
                    backend: backend.vendor.clone(),
                    reason: "deferred again in the background".to_owned(),
                });
            }
            Ok(Err(e)) => {
                return Err(DomainError::RequiredBackendFailed {
                    backend: backend.vendor.clone(),
                    reason: e.to_string(),
                });
            }
            Err(_elapsed) => {
                return Err(DomainError::BackendTimeout {
                    backend: backend.vendor.clone(),
                });
            }
        };
        if detections.is_empty() {
            return Ok(());
        }
        let findings = Self::stamp(backend, detections);

        let mut state = scan.state.lock().await;
        let known = state.considered.len();
        let mut all = std::mem::take(&mut state.considered);
        all.extend(findings);
        let (action, considered) = aggregate_verdict(&all, &scan.policy);
        // Aggregation keeps order and the earlier findings pass it again, so
        // the new survivors are the tail.
        let new = considered.get(known..).unwrap_or_default();
        let built = build_rows(
            &scan.facts,
            action,
            new,
            &scan.input.payload,
            state.next_ordinal,
            &self.settings.evidence,
        );
        state.next_ordinal = state
            .next_ordinal
            .saturating_add(u16::try_from(built.rows.len()).unwrap_or(u16::MAX));
        state.considered = considered;
        drop(state);

        self.store(&scan.scope, built);
        Ok(())
    }

    // ── Reads and deletes ────────────────────────────────────────────

    /// The verdict a scan's stored rows describe: the highest `action`
    /// across them (deferred findings append rows with a later, possibly
    /// higher verdict) and their findings in ordinal order.
    fn verdict_from_rows(rows: Vec<EvidenceRow>) -> Option<ScanVerdict> {
        let first = rows.first()?;
        let (trace_id, tenant_id, created_at) = (first.trace_id, first.tenant_id, first.created_at);
        let action = rows.iter().map(|r| r.action).max()?;
        Some(ScanVerdict {
            trace_id,
            tenant_id,
            action,
            findings: rows.into_iter().map(|r| r.finding).collect(),
            redacted_payload: None,
            // Not stored: only the synchronous verdict carries it.
            unscored_backends: Vec::new(),
            created_at,
        })
    }

    /// Fetch a flagged scan's stored findings by trace id, under the PDP's
    /// scope for the caller's tenant.
    #[instrument(skip(self, ctx), fields(trace_id = %trace_id))]
    pub async fn get_verdict(
        &self,
        ctx: &SecurityContext,
        trace_id: Uuid,
    ) -> Result<ScanVerdict, DomainError> {
        let (_, scope) = self
            .authorize_records(ctx, actions::GET, Some(trace_id))
            .await?;
        let conn = self.db.conn()?;
        let rows = self
            .repo
            .find_by_trace(&conn, &scope, trace_id, Utc::now())
            .await?;
        Self::verdict_from_rows(rows).ok_or_else(|| DomainError::verdict_not_found(trace_id))
    }

    /// Every stored scan of the caller's tenant whose request carried
    /// `correlation_id`, newest first.
    #[instrument(skip(self, ctx, correlation_id))]
    pub async fn find_by_correlation_id(
        &self,
        ctx: &SecurityContext,
        correlation_id: &str,
    ) -> Result<Vec<ScanVerdict>, DomainError> {
        Self::validate_correlation_id(correlation_id)?;
        let (_, scope) = self.authorize_records(ctx, actions::GET, None).await?;
        let conn = self.db.conn()?;
        let rows = self
            .repo
            .find_by_correlation(
                &conn,
                &scope,
                correlation_id,
                Utc::now(),
                CORRELATION_LOOKUP_LIMIT,
            )
            .await?;

        // Rows arrive grouped by trace id, newest scan first.
        let mut verdicts = Vec::new();
        let mut group: Vec<EvidenceRow> = Vec::new();
        for row in rows {
            if group.last().is_some_and(|g| g.trace_id != row.trace_id) {
                verdicts.extend(Self::verdict_from_rows(std::mem::take(&mut group)));
            }
            group.push(row);
        }
        verdicts.extend(Self::verdict_from_rows(group));
        Ok(verdicts)
    }

    /// Remove a scan's stored findings and evidence now (manual evidence
    /// removal). A hard delete. Returns the rows removed.
    #[instrument(skip(self, ctx), fields(trace_id = %trace_id))]
    pub async fn delete_evidence(
        &self,
        ctx: &SecurityContext,
        trace_id: Uuid,
    ) -> Result<u64, DomainError> {
        let (_, scope) = self
            .authorize_records(ctx, actions::DELETE, Some(trace_id))
            .await?;
        let conn = self.db.conn()?;
        let deleted = self.repo.delete_by_trace(&conn, &scope, trace_id).await?;
        if deleted == 0 {
            return Err(DomainError::verdict_not_found(trace_id));
        }
        tracing::info!(trace_id = %trace_id, rows = deleted, "evidence removed on request");
        Ok(deleted)
    }

    // ── Policies ─────────────────────────────────────────────────────

    /// The policy to use when resolution failed under `fail_mode: open`, or
    /// nothing is configured for the tenant: the config fallback, else the
    /// permissive default.
    async fn fallback_policy(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<TenantPolicy, DomainError> {
        if let Some(provider) = &self.default_policy_provider
            && let Some(policy) = provider.get_policy(ctx, tenant_id).await?
        {
            return Ok(policy);
        }
        Ok(TenantPolicy::default_for_tenant(tenant_id))
    }

    /// Resolve the policy for `tenant_id` via the currently authoritative
    /// [`PolicyProvider`] (see `resolve_policy_provider`). A provider with
    /// nothing configured for the tenant gets the permissive default; one
    /// that serves an invalid policy is a resolution failure (`fail_mode`).
    async fn load_policy(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<TenantPolicy, DomainError> {
        let Some(provider) = self.resolve_policy_provider(tenant_id).await? else {
            return Ok(TenantPolicy::default_for_tenant(tenant_id));
        };
        let Some(mut policy) = provider.get_policy(ctx, tenant_id).await? else {
            return Ok(TenantPolicy::default_for_tenant(tenant_id));
        };
        policy.tenant_id = tenant_id;
        if let Err(e) = Self::validate_policy(&policy) {
            self.policy_resolution_failed(
                tenant_id,
                format!("the provider served an invalid policy: {e}"),
            )?;
            return self.fallback_policy(ctx, tenant_id).await;
        }
        Ok(policy)
    }

    /// Get the calling tenant's policy (the permissive default if none has
    /// been configured). Needs `get` on the tenant policy.
    #[instrument(skip(self, ctx))]
    pub async fn get_policy(&self, ctx: &SecurityContext) -> Result<TenantPolicy, DomainError> {
        let tenant_id = self
            .authorize_tenant_resource(ctx, &resources::TENANT_POLICY, actions::GET)
            .await?;
        self.load_policy(ctx, tenant_id).await
    }

    /// Replace the calling tenant's policy, via the currently authoritative
    /// [`PolicyProvider`]. Needs `update` on the tenant policy. The policy's
    /// `tenant_id` may be nil (filled in) or the caller's own; another is
    /// refused.
    ///
    /// # Errors
    ///
    /// `Unsupported` when the provider is read-only or there is none.
    #[instrument(skip(self, ctx, policy))]
    pub async fn put_policy(
        &self,
        ctx: &SecurityContext,
        mut policy: TenantPolicy,
    ) -> Result<TenantPolicy, DomainError> {
        let tenant_id = self
            .authorize_tenant_resource(ctx, &resources::TENANT_POLICY, actions::UPDATE)
            .await?;
        if !policy.tenant_id.is_nil() && policy.tenant_id != tenant_id {
            return Err(DomainError::validation(
                "tenant_id",
                "must be the caller's own tenant",
            ));
        }
        policy.tenant_id = tenant_id;
        Self::validate_policy(&policy)?;

        let Some(provider) = self.resolve_policy_provider(tenant_id).await? else {
            return Err(DomainError::unsupported("put_policy"));
        };
        Ok(provider.put_policy(ctx, policy).await?)
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;

/// A request the SDK's validation refused, as a domain validation error.
fn invalid_request(e: InvalidScanRequest) -> DomainError {
    DomainError::validation(e.field, e.message)
}

/// At most 64 characters of `s`, for an error message.
fn truncate(s: &str) -> String {
    s.chars().take(64).collect()
}
