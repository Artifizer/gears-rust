//! The ONLY file in this module that touches `ort` types. Session load (with
//! graph-contract + SHA-256 verification), a pool of replicas per model
//! (see `docs/runtime/DESIGN.md`), and generic tensor in/out — no model-family
//! decoding logic belongs here.
//!
//! The session itself — one pinned `ort`, an open under a deadline on an
//! abandonable thread, exclusive use of one session — is the toolkit's
//! (`toolkit_onnx_runtime::OnnxSession`). What this file adds on top is what
//! that crate deliberately leaves to the gear: the replica pool, integrity and
//! contract verification, input bounds, the per-inference timeout and load
//! shedding, and rebuilding a replica lost to a panic.
//!
//! # Concurrency model
//!
//! ONNX Runtime's `Run` is not thread-safe — `ort` takes `&mut self` on
//! [`Session::run`] deliberately — so a replica must be held exclusively for
//! the duration of an inference. Replicas are the only concurrency mechanism
//! available, and each is guarded by a [`tokio::sync::Mutex`] rather than a
//! `std::sync::Mutex`: callers queue fairly on the async side instead of
//! parking an executor thread, and the guard is `'static` so it can move into
//! [`tokio::task::spawn_blocking`], where the CPU-bound work belongs.
//!
//! Each slot holds an `Option<Session>`. The blocking task takes the session
//! out, runs, and puts it back. A panic inside the inference is caught there:
//! the session, whose ONNX state is now unknown, is dropped, and the replica —
//! its mutex guard and its pool lease together — is handed to a *rebuild
//! supervisor*, a background task that reopens it from the same verified spec
//! with backoff. Callers never rebuild inline, so a rebuild is never cut short
//! by somebody's inference deadline, and a replica that cannot be rebuilt is
//! retired from the pool rather than retried on every request.
//!
//! # Integrity
//!
//! The toolkit opens sessions by path, so the file is hashed before the
//! sessions are opened **and again after**; a file swapped in between fails
//! the load as `checksum_mismatch`. A rebuild repeats both checks.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ort::session::Session;
use ort::value::{Tensor, Value};
use tokio::sync::{Mutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use toolkit_onnx_runtime::ort;
use toolkit_onnx_runtime::{OnnxSession, OpenError, SessionOptions};
use toolkit_security::crypto::hash::Sha256;

use super::config::{
    Checksum, ConfigError, ModelConfig, OutputDtype, OverloadPolicy, Sha256Digest,
};
use super::metrics_port::{InferenceOutcome, InferenceTiming, NoopMetrics, RuntimeMetricsPort};
use super::pool::{ReplicaLease, ReplicaPool};
use super::registry::{
    GraphContract, InputLimits, RegistryError, SortedNames, validate_tensor, verify_contract,
    verify_input_names,
};
use super::tokenizer::TokenizerLoadError;
use super::{
    LoadedModelInfo, ModelFailureReason, ModelInfo, ModelIntegrity, NamedTensor, RuntimeError,
    TensorData,
};

/// How long one session may take to open before the runtime is declared hung.
///
/// Per session, and generous: a healthy open maps a several-hundred-megabyte
/// model and an unoptimized build is slower still. The hang never completes,
/// so the only cost of a generous budget is how long a genuinely wedged
/// startup takes to report itself. Bounds every rebuild attempt too.
const SESSION_OPEN_TIMEOUT: Duration = Duration::from_mins(1);

/// Read buffer for hashing a model file without holding it in memory.
const HASH_CHUNK_BYTES: usize = 1024 * 1024;

/// SHA-256 of an in-memory buffer.
pub(super) fn sha256_of(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::from_bytes(Sha256::digest(bytes))
}

/// SHA-256 of a file, streamed in fixed-size chunks. Blocking.
fn hash_file(path: &Path) -> std::io::Result<Sha256Digest> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0_u8; HASH_CHUNK_BYTES];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(buf.get(..read).unwrap_or_default());
    }
    Ok(Sha256Digest::from_bytes(hasher.finalize()))
}

/// The typed detail behind a [`LoadFailure`]. For the operator's log; the
/// stable classification consumers see is [`LoadFailure::reason`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadFailureSource {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} hashes to {actual}, expected {expected}")]
    ChecksumMismatch {
        path: PathBuf,
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error(transparent)]
    Session(#[from] OpenError),
    #[error(transparent)]
    Contract(#[from] RegistryError),
    #[error(transparent)]
    Tokenizer(#[from] TokenizerLoadError),
    #[error("a blocking load task did not complete")]
    Task(#[from] tokio::task::JoinError),
}

/// A load failure, with the classification a consumer branches on.
///
/// The `reason` is stable and payload-free; `source` carries the detail, for
/// the operator reading startup logs.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct LoadFailure {
    pub reason: ModelFailureReason,
    #[source]
    pub source: Box<LoadFailureSource>,
}

impl LoadFailure {
    pub(super) fn new(reason: ModelFailureReason, source: impl Into<LoadFailureSource>) -> Self {
        Self {
            reason,
            source: Box::new(source.into()),
        }
    }
}

/// Why [`LoadedModel::load`] did not produce a model.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// This model cannot be served; others may be. See [`LoadFailure`].
    #[error(transparent)]
    Failed(LoadFailure),
    /// ONNX Runtime did not load in time. The thread that tried is abandoned,
    /// so the process must terminate rather than carry on or retry: this is
    /// never a per-model failure.
    #[error(
        "ONNX Runtime did not load within {seconds}s (check ORT_DYLIB_PATH); the process must \
         terminate"
    )]
    RuntimeHung { seconds: u64 },
}

impl From<LoadFailure> for LoadError {
    fn from(failure: LoadFailure) -> Self {
        Self::Failed(failure)
    }
}

fn open_error(e: OpenError) -> LoadError {
    match e {
        OpenError::RuntimeHung { seconds } => LoadError::RuntimeHung { seconds },
        other => LoadFailure::new(ModelFailureReason::SessionBuildFailed, other).into(),
    }
}

/// One replica slot. `None` only while its rebuild supervisor holds it.
type Replica = Arc<Mutex<Option<OnnxSession>>>;

/// `ort` takes session inputs as `(name, value)` pairs with a borrowed-or-owned
/// name.
type SessionInput = (std::borrow::Cow<'static, str>, Value);

/// Everything needed to (re)build a session for one model, kept so a replica
/// rebuilt after a panic gets exactly the settings — and the integrity check —
/// the startup build used.
struct SessionSpec {
    model_id: String,
    model_path: PathBuf,
    intra_threads: usize,
    expected: Option<Sha256Digest>,
}

/// Maps ONNX Runtime's element type onto the dtypes this gear can extract.
/// `None` means the graph emits something we have no extraction path for,
/// which is a load-time failure rather than a runtime surprise.
fn dtype_of(ty: ort::value::TensorElementType) -> Option<OutputDtype> {
    use ort::value::TensorElementType as T;
    match ty {
        T::Float32 => Some(OutputDtype::F32),
        T::Int64 => Some(OutputDtype::I64),
        T::Uint8 => Some(OutputDtype::U8),
        T::Bool => Some(OutputDtype::Bool),
        _ => None,
    }
}

/// Verifies every declared output dtype against what the graph actually emits.
///
/// This is the load-time half of "no silent reinterpretation": the session's
/// own output metadata is available here, so a model declaring `f32` for an
/// output the graph emits as `i64` fails at startup rather than handing a
/// caller plausible, catastrophically wrong numbers.
fn verify_output_dtypes(
    model_id: &str,
    session: &Session,
    declared: &BTreeMap<String, OutputDtype>,
) -> Result<(), RegistryError> {
    let mismatch = |message: String| RegistryError {
        model_id: model_id.to_owned(),
        message,
    };
    for output in session.outputs() {
        let name = output.name();
        let Some(want) = declared.get(name).copied() else {
            return Err(mismatch(format!(
                "output '{name}' has no `output_dtypes` entry; every declared output needs \
                 one, because guessing is how a tensor gets reinterpreted"
            )));
        };

        let ort::value::ValueType::Tensor { ty, .. } = output.dtype() else {
            return Err(mismatch(format!(
                "output '{name}' is not a tensor; this gear only handles tensors"
            )));
        };

        match dtype_of(*ty) {
            Some(actual) if actual == want => {}
            Some(actual) => {
                return Err(mismatch(format!(
                    "output '{name}' is declared {want:?} but the graph emits {actual:?}"
                )));
            }
            None => {
                return Err(mismatch(format!(
                    "output '{name}' has element type {ty:?}, which this gear cannot extract"
                )));
            }
        }
    }
    Ok(())
}

/// Parses `id2label` from a model's metadata JSON into class-id order.
///
/// Deliberately infallible from the caller's point of view: this is optional
/// metadata, and a model that runs correctly must not be refused because a
/// sidecar JSON is malformed. Every failure path returns `None` and logs, so
/// the model still serves — a consumer that needs labels checks for them and
/// says so in its own terms.
///
/// A non-contiguous map is rejected rather than padded. Consumers index this
/// positionally by class id, so a gap would silently shift every label after
/// it — mislabelling is worse than having no labels at all.
pub(super) fn parse_labels(raw: &str, model_id: &str) -> Option<Vec<String>> {
    let parsed: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(model_id, error = %e, "labels file is not valid JSON; no labels");
            return None;
        }
    };

    let map = parsed.get("id2label")?.as_object()?;

    let mut indexed: Vec<(usize, String)> = map
        .iter()
        .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_str()?.to_owned())))
        .collect();
    indexed.sort_unstable_by_key(|(i, _)| *i);

    if indexed.len() != map.len() || !indexed.iter().enumerate().all(|(n, (i, _))| n == *i) {
        tracing::warn!(
            model_id,
            entries = map.len(),
            "id2label is not a contiguous 0..n map; no labels"
        );
        return None;
    }

    Some(indexed.into_iter().map(|(_, label)| label).collect())
}

/// Reads and parses the labels sidecar. Blocking; an absent file is normal
/// and yields `None` silently.
pub(super) fn read_labels(path: &Path, model_id: &str) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(path).ok()?;
    parse_labels(&raw, model_id)
}

/// Checks the model file before any session is opened: readable, and — when
/// pinned — hashing to the configured digest. Streams the file on the
/// blocking pool. Returns the digest that was verified, if any.
///
/// # Errors
///
/// `file_unreadable` or `checksum_mismatch`.
pub(super) async fn check_model_file(
    cfg: &ModelConfig,
) -> Result<Option<Sha256Digest>, LoadFailure> {
    let path = cfg.model_dir.join(&cfg.model_file);
    let expected = match cfg.checksum {
        Checksum::Sha256(digest) => Some(digest),
        Checksum::Skip => {
            tracing::warn!(
                model_id = %cfg.model_id,
                "onnx-scanner: skip_checksum is set; this model's integrity is NOT verified \
                 (local development only)"
            );
            None
        }
    };
    verify_file(path, expected).await?;
    Ok(expected)
}

/// Hashes `path` (or, unpinned, just opens it) on the blocking pool and
/// compares against `expected`.
async fn verify_file(path: PathBuf, expected: Option<Sha256Digest>) -> Result<(), LoadFailure> {
    let (path, checked) = tokio::task::spawn_blocking(move || {
        let checked = match expected {
            Some(expected) => {
                hash_file(&path).map(|actual| (actual != expected).then_some((expected, actual)))
            }
            None => std::fs::File::open(&path).map(|_| None),
        };
        (path, checked)
    })
    .await
    .map_err(|e| LoadFailure::new(ModelFailureReason::FileUnreadable, e))?;
    match checked {
        Ok(None) => Ok(()),
        Ok(Some((expected, actual))) => Err(LoadFailure::new(
            ModelFailureReason::ChecksumMismatch,
            LoadFailureSource::ChecksumMismatch {
                path,
                expected,
                actual,
            },
        )),
        Err(source) => Err(LoadFailure::new(
            ModelFailureReason::FileUnreadable,
            LoadFailureSource::Io { path, source },
        )),
    }
}

/// Opens one session through the toolkit: on an abandonable thread, under a
/// deadline, so a bad `ORT_DYLIB_PATH` cannot wedge the process.
async fn open_session(spec: &SessionSpec) -> Result<OnnxSession, OpenError> {
    let mut options = SessionOptions::new(&spec.model_path);
    options.intra_op_threads = Some(spec.intra_threads);
    options.load_timeout = SESSION_OPEN_TIMEOUT;
    options.thread_name = format!("onnx-init-{}", spec.model_id);
    OnnxSession::open(&options).await
}

/// A rebuild's open: hash, open, hash again — the same integrity the startup
/// load gets.
async fn open_verified(spec: &SessionSpec) -> Result<OnnxSession, LoadError> {
    verify_file(spec.model_path.clone(), spec.expected).await?;
    let session = open_session(spec).await.map_err(open_error)?;
    verify_file(spec.model_path.clone(), spec.expected).await?;
    Ok(session)
}

/// The input names a model file declares, as the runtime sees them.
///
/// Opens a throwaway session at the same settings a served one gets, because
/// the declared inputs can depend on the graph optimization level the session
/// is built at — the answer is the runtime's, not something to probe from the
/// file. For tooling that has to state a contract before a model is configured
/// (the quality evaluation); the host itself reads the same names off the
/// session it serves.
///
/// # Errors
///
/// [`OpenError`] when the runtime refuses the file or does not load in time.
pub async fn declared_input_names(model_path: &Path) -> Result<Vec<String>, OpenError> {
    let mut options = SessionOptions::new(model_path);
    options.load_timeout = SESSION_OPEN_TIMEOUT;
    let session = OnnxSession::open(&options).await?;
    Ok(session
        .run(|session| {
            session
                .inputs()
                .iter()
                .map(|input| input.name().to_owned())
                .collect()
        })
        .await)
}

// --- Rebuild supervision ---------------------------------------------------

/// How hard a supervisor tries before retiring a replica.
#[derive(Debug, Clone, Copy)]
pub(super) struct RebuildPolicy {
    pub(super) max_attempts: u32,
    pub(super) initial_backoff: Duration,
    pub(super) max_backoff: Duration,
}

impl RebuildPolicy {
    /// Five attempts over roughly 15 s, each bounded by the session-open
    /// deadline.
    pub(super) const DEFAULT: Self = Self {
        max_attempts: 5,
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(30),
    };
}

/// How a rebuild ended.
#[derive(Debug)]
pub(super) enum RebuildOutcome<S> {
    Rebuilt(S),
    /// Every attempt failed honestly; the replica is retired.
    GaveUp {
        attempts: u32,
    },
    /// The runtime hung opening a session (now or for an earlier replica).
    /// Terminal: every further attempt would abandon another thread.
    RuntimeHung,
    /// The model is being dropped.
    Cancelled,
}

/// Retries `open` with exponential backoff until it succeeds, fails
/// `max_attempts` times, hangs, or `shutdown` fires.
pub(super) async fn rebuild_with_backoff<S, F, Fut>(
    model_id: &str,
    policy: RebuildPolicy,
    shutdown: &CancellationToken,
    runtime_hung: &AtomicBool,
    mut open: F,
) -> RebuildOutcome<S>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<S, LoadError>>,
{
    let mut backoff = policy.initial_backoff;
    for attempt in 1..=policy.max_attempts {
        if runtime_hung.load(Ordering::Acquire) {
            return RebuildOutcome::RuntimeHung;
        }
        let result = tokio::select! {
            () = shutdown.cancelled() => return RebuildOutcome::Cancelled,
            result = open() => result,
        };
        let failure = match result {
            Ok(session) => return RebuildOutcome::Rebuilt(session),
            Err(LoadError::RuntimeHung { seconds }) => {
                runtime_hung.store(true, Ordering::Release);
                log_rebuild_hung(model_id, seconds);
                return RebuildOutcome::RuntimeHung;
            }
            Err(LoadError::Failed(failure)) => failure,
        };
        if attempt == policy.max_attempts {
            log_rebuild_failure(model_id, attempt, policy.max_attempts, None, &failure);
            break;
        }
        log_rebuild_failure(
            model_id,
            attempt,
            policy.max_attempts,
            Some(backoff),
            &failure,
        );
        tokio::select! {
            () = shutdown.cancelled() => return RebuildOutcome::Cancelled,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = backoff.saturating_mul(2).min(policy.max_backoff);
    }
    RebuildOutcome::GaveUp {
        attempts: policy.max_attempts,
    }
}

fn log_rebuild_hung(model_id: &str, seconds: u64) {
    tracing::error!(
        model_id,
        seconds,
        "onnx-scanner: ONNX Runtime hung while rebuilding a replica; its open thread is \
         abandoned and this model will not attempt another rebuild"
    );
}

fn log_rebuild_failure(
    model_id: &str,
    attempt: u32,
    max_attempts: u32,
    retry_in: Option<Duration>,
    failure: &LoadFailure,
) {
    let retry_in_ms = retry_in.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    tracing::warn!(
        model_id,
        attempt,
        max_attempts,
        retry_in_ms,
        reason = failure.reason.as_code(),
        error = %failure,
        "onnx-scanner: replica rebuild failed"
    );
}

/// Replica bookkeeping shared by the request path and the supervisors.
pub(super) struct ReplicaHealth {
    total: usize,
    retired: AtomicUsize,
    rebuilding: AtomicUsize,
    unavailable: AtomicBool,
    runtime_hung: AtomicBool,
}

impl ReplicaHealth {
    pub(super) const fn new(total: usize) -> Self {
        Self {
            total,
            retired: AtomicUsize::new(0),
            rebuilding: AtomicUsize::new(0),
            unavailable: AtomicBool::new(false),
            runtime_hung: AtomicBool::new(false),
        }
    }

    /// Replicas holding a session right now (busy ones included).
    pub(super) fn live(&self) -> usize {
        self.total.saturating_sub(
            self.retired
                .load(Ordering::Acquire)
                .saturating_add(self.rebuilding.load(Ordering::Acquire)),
        )
    }

    pub(super) fn is_unavailable(&self) -> bool {
        self.unavailable.load(Ordering::Acquire)
    }

    /// Records one replica gone for good. `true` when that was the last one
    /// and the model is now unavailable. One-way: nothing here can tell
    /// whether the cause was fixed, so recovery is a restart.
    pub(super) fn retire(&self) -> bool {
        let retired = self
            .retired
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let last = retired >= self.total;
        if last {
            self.unavailable.store(true, Ordering::Release);
        }
        last
    }
}

/// Applies a rebuild's outcome: restore the replica to the pool, or retire it
/// (closing the pool when it was the last). Generic over the session so the
/// transitions are testable without a runtime.
pub(super) fn settle_rebuild<S>(
    model_id: &str,
    health: &ReplicaHealth,
    pool: &ReplicaPool,
    metrics: &dyn RuntimeMetricsPort,
    mut guard: OwnedMutexGuard<Option<S>>,
    lease: ReplicaLease,
    outcome: RebuildOutcome<S>,
) {
    let replica = lease.index();
    let retire = match outcome {
        RebuildOutcome::Rebuilt(session) => {
            *guard = Some(session);
            metrics.record_session_rebuild(model_id, true);
            tracing::info!(model_id, replica, "onnx-scanner: replica rebuilt");
            false
        }
        RebuildOutcome::Cancelled => true,
        RebuildOutcome::GaveUp { attempts } => {
            metrics.record_session_rebuild(model_id, false);
            tracing::warn!(
                model_id,
                replica,
                attempts,
                "onnx-scanner: replica rebuild gave up"
            );
            true
        }
        RebuildOutcome::RuntimeHung => {
            metrics.record_session_rebuild(model_id, false);
            true
        }
    };
    // Guard before lease: the replica re-enters the pool only once its mutex
    // is free.
    drop(guard);
    if retire {
        lease.retire();
        retire_replica(model_id, replica, health, pool);
    } else {
        drop(lease);
    }
    health.rebuilding.fetch_sub(1, Ordering::AcqRel);
}

/// Records a retired replica, closing the model when it was the last.
fn retire_replica(model_id: &str, replica: usize, health: &ReplicaHealth, pool: &ReplicaPool) {
    if health.retire() {
        pool.close();
        tracing::error!(
            model_id,
            reason = ModelFailureReason::SessionBuildFailed.as_code(),
            "onnx-scanner: model lost every replica and could not rebuild; it is now unavailable"
        );
    } else {
        tracing::warn!(model_id, replica, "onnx-scanner: replica retired");
    }
}

// --- Load shedding -----------------------------------------------------------

/// The per-model in-flight ceiling and what to do at it.
pub(super) struct InFlightLimit {
    permits: Arc<Semaphore>,
    max: usize,
    policy: OverloadPolicy,
    queue_timeout: Duration,
}

impl InFlightLimit {
    pub(super) fn new(max: NonZeroUsize, policy: OverloadPolicy, queue_timeout: Duration) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max.get())),
            max: max.get(),
            policy,
            queue_timeout,
        }
    }

    /// Takes an in-flight permit, or sheds the call as
    /// [`RuntimeError::Overloaded`] reporting the configured ceiling.
    pub(super) async fn acquire(
        &self,
        model_id: &str,
    ) -> Result<OwnedSemaphorePermit, RuntimeError> {
        let overloaded = || RuntimeError::Overloaded {
            model_id: model_id.to_owned(),
            max_in_flight: self.max,
        };
        match self.policy {
            OverloadPolicy::Reject => Arc::clone(&self.permits)
                .try_acquire_owned()
                .map_err(|_| overloaded()),
            // Bounded by the same deadline the inference gets, so a queued
            // caller cannot wait longer than a running one is allowed to take.
            OverloadPolicy::Queue => {
                match tokio::time::timeout(
                    self.queue_timeout,
                    Arc::clone(&self.permits).acquire_owned(),
                )
                .await
                {
                    Ok(Ok(permit)) => Ok(permit),
                    // The semaphore is never closed; treat the impossible
                    // branch as capacity exhaustion rather than panicking.
                    Ok(Err(_)) | Err(_) => Err(overloaded()),
                }
            }
        }
    }
}

// --- The model ---------------------------------------------------------------

/// What the request path and the rebuild supervisors share.
struct Shared {
    spec: SessionSpec,
    replicas: Box<[Replica]>,
    /// Which replicas are free. Built from `replicas.len()`, which never
    /// changes, so every index it leases is in bounds.
    pool: Arc<ReplicaPool>,
    health: ReplicaHealth,
    metrics: Arc<dyn RuntimeMetricsPort>,
    output_names: Box<[String]>,
    output_dtypes: BTreeMap<String, OutputDtype>,
    /// Cancelled when the model is dropped, so no supervisor outlives it.
    shutdown: CancellationToken,
    rebuild: RebuildPolicy,
}

impl Shared {
    fn unavailable(&self) -> RuntimeError {
        RuntimeError::ModelUnavailable {
            model_id: self.spec.model_id.clone(),
            reason: ModelFailureReason::SessionBuildFailed,
        }
    }

    /// Hands an emptied replica to a background supervisor.
    fn spawn_rebuild(
        self: Arc<Self>,
        runtime: &tokio::runtime::Handle,
        guard: OwnedMutexGuard<Option<OnnxSession>>,
        lease: ReplicaLease,
    ) {
        self.health.rebuilding.fetch_add(1, Ordering::AcqRel);
        // Detached on purpose: the supervisor is bounded by the open deadline
        // per attempt and by `shutdown`, and owns everything it touches.
        drop(runtime.spawn(async move {
            let outcome = rebuild_with_backoff(
                &self.spec.model_id,
                self.rebuild,
                &self.shutdown,
                &self.health.runtime_hung,
                || open_verified(&self.spec),
            )
            .await;
            settle_rebuild(
                &self.spec.model_id,
                &self.health,
                &self.pool,
                &*self.metrics,
                guard,
                lease,
                outcome,
            );
        }));
    }
}

/// One configured model: N loaded session replicas, each call dispatched to
/// whichever replica is free (see `docs/runtime/DESIGN.md` §2.2 and
/// `pool.rs`).
pub struct LoadedModel {
    shared: Arc<Shared>,
    /// Recorded at load; `replicas` is replaced by the live count in
    /// [`Self::snapshot`].
    info: LoadedModelInfo,
    input_names: SortedNames,
    limits: InputLimits,
    inference_timeout: Duration,
    in_flight: InFlightLimit,
}

impl Drop for LoadedModel {
    fn drop(&mut self) {
        self.shared.shutdown.cancel();
    }
}

impl LoadedModel {
    /// Validates the config, verifies the model file, reads its labels and
    /// opens `cfg.replicas` sessions, recording no metrics.
    ///
    /// # Errors
    ///
    /// See [`LoadError`].
    pub async fn load(cfg: &ModelConfig) -> Result<Self, LoadError> {
        Self::load_with_metrics(cfg, Arc::new(NoopMetrics)).await
    }

    /// [`Self::load`], recording into `metrics`.
    ///
    /// # Errors
    ///
    /// See [`LoadError`].
    pub async fn load_with_metrics(
        cfg: &ModelConfig,
        metrics: Arc<dyn RuntimeMetricsPort>,
    ) -> Result<Self, LoadError> {
        cfg.validate()
            .map_err(|e| LoadFailure::new(ModelFailureReason::InvalidConfig, e))?;
        let verified = check_model_file(cfg).await?;
        let labels_path = cfg.model_dir.join(&cfg.labels_file);
        let model_id = cfg.model_id.clone();
        let labels = tokio::task::spawn_blocking(move || read_labels(&labels_path, &model_id))
            .await
            .map_err(|e| LoadFailure::new(ModelFailureReason::FileUnreadable, e))?;
        Self::open(cfg, verified, labels, metrics).await
    }

    /// Opens the sessions for a config that has already been validated and
    /// whose file was checked by [`check_model_file`], then checks the file
    /// again.
    pub(super) async fn open(
        cfg: &ModelConfig,
        verified: Option<Sha256Digest>,
        labels: Option<Vec<String>>,
        metrics: Arc<dyn RuntimeMetricsPort>,
    ) -> Result<Self, LoadError> {
        let replica_count = NonZeroUsize::new(cfg.replicas).ok_or_else(|| {
            LoadFailure::new(
                ModelFailureReason::InvalidConfig,
                ConfigError::OutOfRange {
                    model_id: cfg.model_id.clone(),
                    field: "replicas",
                    value: 0,
                    min: 1,
                    max: u128::MAX,
                },
            )
        })?;
        let spec = SessionSpec {
            model_id: cfg.model_id.clone(),
            model_path: cfg.model_dir.join(&cfg.model_file),
            intra_threads: cfg.intra_threads,
            expected: verified,
        };

        let mut replicas = Vec::with_capacity(replica_count.get());
        let mut actual = GraphContract::default();
        for i in 0..replica_count.get() {
            let session = open_session(&spec).await.map_err(open_error)?;
            if i == 0 {
                // The graph's own metadata: the contract is checked against
                // what the runtime reports, never against what the file name
                // promises.
                actual = session
                    .run(|session| {
                        verify_output_dtypes(&cfg.model_id, session, &cfg.output_dtypes).map(|()| {
                            GraphContract {
                                inputs: session
                                    .inputs()
                                    .iter()
                                    .map(|input| input.name().to_owned())
                                    .collect(),
                                outputs: session
                                    .outputs()
                                    .iter()
                                    .map(|output| output.name().to_owned())
                                    .collect(),
                            }
                        })
                    })
                    .await
                    .map_err(|e| LoadFailure::new(ModelFailureReason::ContractMismatch, e))?;
                let expected = GraphContract {
                    inputs: cfg.expected_inputs.clone(),
                    outputs: cfg.expected_outputs.clone(),
                };
                verify_contract(&cfg.model_id, &expected, &actual)
                    .map_err(|e| LoadFailure::new(ModelFailureReason::ContractMismatch, e))?;
            }
            replicas.push(Arc::new(Mutex::new(Some(session))));
        }

        // The toolkit opened the file by path after we hashed it; hash it
        // again so a swap in between is a failed load, not a served model.
        verify_file(spec.model_path.clone(), spec.expected).await?;

        let integrity = verified.map_or(ModelIntegrity::Unverified, |digest| {
            ModelIntegrity::Verified {
                sha256: digest.to_hex(),
            }
        });
        let max_in_flight =
            NonZeroUsize::new(cfg.effective_max_in_flight()).unwrap_or(replica_count);
        let GraphContract { inputs, outputs } = actual;
        let info = LoadedModelInfo {
            input_names: inputs,
            output_names: outputs.clone(),
            integrity,
            replicas: replica_count.get(),
            labels,
        };
        Ok(Self {
            shared: Arc::new(Shared {
                spec,
                replicas: replicas.into_boxed_slice(),
                pool: ReplicaPool::new(replica_count),
                health: ReplicaHealth::new(replica_count.get()),
                metrics,
                output_names: outputs.into_boxed_slice(),
                output_dtypes: cfg.output_dtypes.clone(),
                shutdown: CancellationToken::new(),
                rebuild: RebuildPolicy::DEFAULT,
            }),
            info,
            input_names: SortedNames::new(&cfg.expected_inputs),
            limits: InputLimits {
                elements_per_tensor: cfg.max_tensor_elements,
                tensors_per_call: cfg.max_input_tensors,
                extent_per_dimension: cfg.max_dimension,
                text_bytes: cfg.max_text_bytes,
            },
            inference_timeout: cfg.inference_timeout,
            in_flight: InFlightLimit::new(max_in_flight, cfg.on_overload, cfg.inference_timeout),
        })
    }

    /// The configured model id.
    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.shared.spec.model_id
    }

    /// What was recorded at load (`replicas` is the configured count).
    #[must_use]
    pub const fn loaded_info(&self) -> &LoadedModelInfo {
        &self.info
    }

    /// This model's state right now: ready with its live replica count, or
    /// unavailable once every replica has been lost and could not be rebuilt.
    #[must_use]
    pub fn snapshot(&self) -> ModelInfo {
        if self.shared.health.is_unavailable() {
            return ModelInfo::unavailable(self.model_id(), ModelFailureReason::SessionBuildFailed);
        }
        let mut loaded = self.info.clone();
        loaded.replicas = self.live_replicas();
        ModelInfo::ready(self.model_id(), loaded)
    }

    /// How many replicas currently hold a session: configured, minus those
    /// being rebuilt or retired. A busy replica counts as live.
    #[must_use]
    pub fn live_replicas(&self) -> usize {
        self.shared.health.live()
    }

    /// This model's input limits, so the gear can validate `encode` text
    /// without reaching into the session layer.
    #[must_use]
    pub const fn limits(&self) -> InputLimits {
        self.limits
    }

    /// Runs one inference on the first replica that is free.
    ///
    /// Inputs are validated before anything is allocated, the call is bounded
    /// by the model's `inference_timeout`, and concurrency is capped.
    ///
    /// # Cancellation and timeouts
    ///
    /// Cancel-safe with respect to the session: dropping the returned future
    /// does not stop the in-flight inference — its result is simply discarded
    /// — and the replica is released either way, because the blocking task
    /// owns the mutex guard and the pool lease and drops them on completion.
    ///
    /// **A timeout abandons the result; it does not cancel the work.** ONNX
    /// Runtime has no mid-graph cancellation, so after
    /// [`RuntimeError::Timeout`] the replica stays occupied until the
    /// underlying inference finishes on its own. Retrying immediately competes
    /// with work that is still running; a caller that times out repeatedly is
    /// making the problem worse, not shedding it.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::InvalidInput`] for a malformed or oversized input;
    /// [`RuntimeError::ContractMismatch`] if the supplied tensor names do
    /// not match the model's declared inputs; [`RuntimeError::Overloaded`]
    /// when at capacity; [`RuntimeError::Timeout`] on deadline expiry;
    /// [`RuntimeError::ModelUnavailable`] if every replica has been lost;
    /// [`RuntimeError::InferenceFailed`] if the session rejects the inputs,
    /// panics, or an output cannot be extracted as its declared dtype.
    #[tracing::instrument(
        name = "onnx_scanner.runtime.session.run",
        skip_all,
        fields(
            model_id = %self.model_id(),
            tensors = inputs.len(),
            replicas = self.shared.replicas.len(),
            outcome = tracing::field::Empty,
            queue_wait_ms = tracing::field::Empty,
            execution_ms = tracing::field::Empty,
        )
    )]
    pub async fn run(&self, inputs: Vec<NamedTensor>) -> Result<Vec<NamedTensor>, RuntimeError> {
        // `skip_all` above is deliberate and load-bearing: `inputs` carries
        // tensor data. Only the count is recorded, never the values.
        let span = tracing::Span::current();
        self.shared
            .metrics
            .record_in_flight_delta(self.model_id(), 1);
        let finished = InFlightGuard {
            metrics: &*self.shared.metrics,
            model_id: self.model_id(),
        };

        let outcome = self.run_instrumented(inputs, &span).await;
        drop(finished);
        outcome
    }

    /// The body of [`Self::run`], separated so the in-flight guard in the
    /// caller covers every early return including validation failures.
    async fn run_instrumented(
        &self,
        inputs: Vec<NamedTensor>,
        span: &tracing::Span,
    ) -> Result<Vec<NamedTensor>, RuntimeError> {
        let record = |outcome: InferenceOutcome, timing: InferenceTiming| {
            span.record("outcome", outcome.as_label());
            span.record("queue_wait_ms", timing.queue_wait.as_millis());
            span.record("execution_ms", timing.execution.as_millis());
            self.shared
                .metrics
                .record_inference(self.model_id(), outcome, timing);
        };

        if let Err(e) = self.validate_inputs(&inputs) {
            record((&e).into(), InferenceTiming::default());
            return Err(e);
        }

        // Queue wait is measured around permit acquisition alone. That is what
        // makes it distinguishable from execution: it rises with concurrency
        // and replica scarcity, where execution time rises with input size and
        // thread budget. A single total would conceal which one is the problem.
        let queueing = Instant::now();
        let permit = match self.in_flight.acquire(self.model_id()).await {
            Ok(permit) => permit,
            Err(e) => {
                record(
                    (&e).into(),
                    InferenceTiming {
                        queue_wait: queueing.elapsed(),
                        execution: Duration::ZERO,
                    },
                );
                return Err(e);
            }
        };
        let queue_wait = queueing.elapsed();

        let shared = Arc::clone(&self.shared);
        let executing = Instant::now();
        let inference = self.with_replica(move |session| {
            let model_id = &shared.spec.model_id;
            let session_inputs = build_inputs(inputs)?;
            let outputs =
                session
                    .run(session_inputs)
                    .map_err(|e| RuntimeError::InferenceFailed {
                        model_id: model_id.clone(),
                        message: e.to_string(),
                    })?;
            collect_outputs(
                model_id,
                &shared.output_names,
                &shared.output_dtypes,
                &outputs,
            )
        });

        let (result, replica_wait) = match tokio::time::timeout(self.inference_timeout, inference)
            .await
        {
            Ok(Ok((value, replica_wait))) => (Ok(value), replica_wait),
            Ok(Err(e)) => (Err(e), Duration::ZERO),
            Err(_elapsed) => (
                Err(RuntimeError::Timeout {
                    model_id: self.model_id().to_owned(),
                    elapsed_ms: u64::try_from(executing.elapsed().as_millis()).unwrap_or(u64::MAX),
                }),
                Duration::ZERO,
            ),
        };
        drop(permit);

        // Waiting for a replica is queueing, not execution — even though it
        // happens after the permit was granted. Attributing it to execution
        // would make a contended single-replica model look slow rather than
        // oversubscribed, which is the opposite diagnosis.
        let timing = InferenceTiming {
            queue_wait: queue_wait + replica_wait,
            execution: executing.elapsed().saturating_sub(replica_wait),
        };
        match &result {
            Ok(_) => record(InferenceOutcome::Ok, timing),
            Err(e) => record(e.into(), timing),
        }
        result
    }

    /// Rejects malformed or oversized input before any allocation.
    fn validate_inputs(&self, inputs: &[NamedTensor]) -> Result<(), RuntimeError> {
        if inputs.len() > self.limits.tensors_per_call {
            return Err(RuntimeError::InvalidInput {
                model_id: self.model_id().to_owned(),
                tensor: "<call>".to_owned(),
                message: format!(
                    "{} input tensors, over the limit of {}",
                    inputs.len(),
                    self.limits.tensors_per_call
                ),
            });
        }

        for tensor in inputs {
            validate_tensor(&tensor.name, &tensor.shape, tensor.data.len(), self.limits).map_err(
                |rejection| RuntimeError::InvalidInput {
                    model_id: self.model_id().to_owned(),
                    tensor: rejection.tensor,
                    message: rejection.message,
                },
            )?;
        }

        // Names last: a caller that got the shapes wrong should hear about the
        // shapes, which are the more specific complaint.
        let supplied: Vec<&str> = inputs.iter().map(|t| t.name.as_str()).collect();
        verify_input_names(self.model_id(), &self.input_names, &supplied).map_err(|e| {
            RuntimeError::ContractMismatch {
                model_id: e.model_id,
                message: e.message,
            }
        })
    }

    /// Takes exclusive use of one replica and runs `f` on the blocking pool.
    ///
    /// The session never leaves the mutex from a caller's point of view; the
    /// *guard* moves into the closure. That is what makes cancellation safe —
    /// the guard is released by `Drop` when the blocking task ends, whether or
    /// not anyone still awaits the result.
    ///
    /// Returns the closure's result alongside **how long acquiring the replica
    /// took**, so the caller can attribute that time to queueing rather than to
    /// execution.
    async fn with_replica<T, F>(&self, f: F) -> Result<(T, Duration), RuntimeError>
    where
        F: FnOnce(&mut Session) -> Result<T, RuntimeError> + Send + 'static,
        T: Send + 'static,
    {
        let shared = &self.shared;
        // A model that has already lost every replica reports itself
        // unavailable rather than making each caller rediscover it.
        if shared.health.is_unavailable() {
            return Err(shared.unavailable());
        }

        // Fair FIFO queueing for whichever replica frees up first, on the
        // async side rather than on a blocking thread.
        let waiting = Instant::now();
        let Some(lease) = shared.pool.acquire().await else {
            // The pool closes only when the last replica is retired.
            return Err(if shared.health.is_unavailable() {
                shared.unavailable()
            } else {
                RuntimeError::Internal(format!(
                    "replica pool for model {} leased no replica",
                    shared.spec.model_id
                ))
            });
        };
        let replica_wait = waiting.elapsed();

        // The lease makes this replica ours alone, so its mutex is
        // uncontended. `lock_owned` because `spawn_blocking` needs `'static`.
        let replica = shared.replicas.get(lease.index()).cloned().ok_or_else(|| {
            RuntimeError::Internal(format!(
                "replica pool for model {} leased an index out of range",
                shared.spec.model_id
            ))
        })?;
        let guard = replica.lock_owned().await;
        let runtime = tokio::runtime::Handle::current();

        if guard.is_none() {
            // Not expected — an emptied replica stays leased by its
            // supervisor — but never serve from one: hand it back to a
            // supervisor and let this caller retry.
            Arc::clone(shared).spawn_rebuild(&runtime, guard, lease);
            return Err(RuntimeError::InferenceFailed {
                model_id: shared.spec.model_id.clone(),
                message: "replica had no session; it is being rebuilt".to_owned(),
            });
        }

        let shared = Arc::clone(shared);
        tokio::task::spawn_blocking(move || {
            let mut guard = guard;
            let Some(session) = guard.take() else {
                return Err(RuntimeError::Internal(format!(
                    "replica for model {} emptied while leased",
                    shared.spec.model_id
                )));
            };
            // `run_blocking`: this is a blocking-pool thread, and the replica
            // is already held exclusively by `guard`, so the session's own
            // lock is uncontended.
            match std::panic::catch_unwind(AssertUnwindSafe(|| session.run_blocking(f))) {
                Ok(result) => {
                    // Restored on success and on a returned error alike — a
                    // rejected input does not damage the session. Guard
                    // before lease: the replica re-enters the pool only once
                    // its mutex is free, and only when the inference has
                    // actually ended — not when a caller timed out.
                    *guard = Some(session);
                    drop(guard);
                    drop(lease);
                    result.map(|value| (value, replica_wait))
                }
                Err(payload) => {
                    // The session's ONNX state is unknown: never reuse it.
                    drop(session);
                    let model_id = shared.spec.model_id.clone();
                    tracing::error!(
                        model_id = %model_id,
                        replica = lease.index(),
                        panic = panic_message(&*payload),
                        "onnx-scanner: inference panicked; the session is discarded and the \
                         replica is being rebuilt"
                    );
                    shared.spawn_rebuild(&runtime, guard, lease);
                    Err(RuntimeError::InferenceFailed {
                        model_id,
                        message: "inference panicked; the replica is being rebuilt".to_owned(),
                    })
                }
            }
        })
        .await
        .map_err(|join_err| {
            RuntimeError::Internal(format!(
                "inference task for model {} did not complete: {join_err}",
                self.model_id()
            ))
        })?
    }
}

/// One replica held out of the pool until dropped. Test support: lets an
/// integration test saturate a model, or pin which replica serves next,
/// without depending on how long an inference takes.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
#[derive(Debug)]
pub struct ReplicaHold {
    lease: ReplicaLease,
}

#[cfg(any(test, feature = "test-support"))]
impl ReplicaHold {
    /// The held replica's index.
    #[must_use]
    pub const fn replica(&self) -> usize {
        self.lease.index()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl LoadedModel {
    /// Waits for a free replica and holds it. `None` once the model has lost
    /// every replica. Takes no in-flight permit.
    #[doc(hidden)]
    pub async fn hold_replica(&self) -> Option<ReplicaHold> {
        self.shared
            .pool
            .acquire()
            .await
            .map(|lease| ReplicaHold { lease })
    }
}

/// A panic payload as text, when it is text.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

/// Converts caller tensors into session inputs, moving their data.
fn build_inputs(inputs: Vec<NamedTensor>) -> Result<Vec<SessionInput>, RuntimeError> {
    inputs
        .into_iter()
        .map(|tensor| {
            let (name, value) = build_tensor(tensor)?;
            Ok((name.into(), value))
        })
        .collect()
}

fn collect_outputs(
    model_id: &str,
    output_names: &[String],
    output_dtypes: &BTreeMap<String, OutputDtype>,
    outputs: &ort::session::SessionOutputs<'_>,
) -> Result<Vec<NamedTensor>, RuntimeError> {
    let mut result = Vec::with_capacity(output_names.len());
    for name in output_names {
        // No default: `ModelConfig::validate` rejects a config that omits any
        // declared output, and `verify_output_dtypes` proves the declaration
        // matches the graph. An absent entry here means those checks were
        // bypassed, which is a bug, not a tensor to guess at.
        let dtype = output_dtypes.get(name).copied().ok_or_else(|| {
            RuntimeError::Internal(format!(
                "model {model_id}: output '{name}' reached extraction with no declared dtype"
            ))
        })?;
        let value = outputs
            .get(name.as_str())
            .ok_or_else(|| RuntimeError::InferenceFailed {
                model_id: model_id.to_owned(),
                message: format!("session did not return expected output '{name}'"),
            })?;
        result.push(extract_tensor(model_id, name, value, dtype)?);
    }
    Ok(result)
}

fn build_tensor(tensor: NamedTensor) -> Result<(String, Value), RuntimeError> {
    let NamedTensor { name, shape, data } = tensor;
    // Checked rather than asserted in a comment: `validate_tensor` bounds each
    // dimension by `extent_per_dimension`, but that limit is operator-set, so
    // the conversion proves itself instead of trusting configuration.
    let shape: Vec<i64> = shape
        .iter()
        .map(|&d| {
            i64::try_from(d).map_err(|_| {
                RuntimeError::Internal(format!(
                    "tensor '{name}': dimension {d} does not fit in i64"
                ))
            })
        })
        .collect::<Result<_, _>>()?;
    let failed = |dtype: &str, e: ort::Error| {
        RuntimeError::Internal(format!("building {dtype} tensor '{name}': {e}"))
    };
    let value = match data {
        TensorData::I64(data) => Tensor::from_array((shape, data))
            .map_err(|e| failed("I64", e))?
            .into_dyn(),
        TensorData::F32(data) => Tensor::from_array((shape, data))
            .map_err(|e| failed("F32", e))?
            .into_dyn(),
        TensorData::U8(data) => Tensor::from_array((shape, data))
            .map_err(|e| failed("U8", e))?
            .into_dyn(),
        TensorData::Bool(data) => Tensor::from_array((shape, data))
            .map_err(|e| failed("Bool", e))?
            .into_dyn(),
    };
    Ok((name, value))
}

fn extract_tensor(
    model_id: &str,
    name: &str,
    value: &Value,
    dtype: OutputDtype,
) -> Result<NamedTensor, RuntimeError> {
    let fail = |e: ort::Error| RuntimeError::InferenceFailed {
        model_id: model_id.to_owned(),
        message: format!("extracting output '{name}': {e}"),
    };
    let (shape, data) = match dtype {
        OutputDtype::F32 => {
            let (shape, slice) = value.try_extract_tensor::<f32>().map_err(fail)?;
            (shape, TensorData::F32(slice.to_vec()))
        }
        OutputDtype::I64 => {
            let (shape, slice) = value.try_extract_tensor::<i64>().map_err(fail)?;
            (shape, TensorData::I64(slice.to_vec()))
        }
        OutputDtype::U8 => {
            let (shape, slice) = value.try_extract_tensor::<u8>().map_err(fail)?;
            (shape, TensorData::U8(slice.to_vec()))
        }
        OutputDtype::Bool => {
            let (shape, slice) = value.try_extract_tensor::<bool>().map_err(fail)?;
            (shape, TensorData::Bool(slice.to_vec()))
        }
    };
    // A materialized tensor's dims are non-negative, but that is ONNX
    // Runtime's invariant rather than ours, so it is checked here.
    let shape = shape
        .iter()
        .map(|&d| {
            usize::try_from(d).map_err(|_| RuntimeError::InferenceFailed {
                model_id: model_id.to_owned(),
                message: format!("output '{name}' reported a negative dimension {d}"),
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(NamedTensor {
        name: name.to_owned(),
        shape,
        data,
    })
}

/// Decrements the in-flight gauge on drop, so a cancelled call cannot leave it
/// permanently elevated — the failure mode that makes a gauge untrustworthy.
struct InFlightGuard<'a> {
    metrics: &'a dyn RuntimeMetricsPort,
    model_id: &'a str,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.metrics.record_in_flight_delta(self.model_id, -1);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex as StdMutex;

    use super::*;
    use crate::runtime::metrics_port::LoadOutcome;

    fn failure() -> LoadError {
        LoadFailure::new(
            ModelFailureReason::SessionBuildFailed,
            OpenError::Session("simulated refusal".to_owned()),
        )
        .into()
    }

    fn fast_policy(max_attempts: u32) -> RebuildPolicy {
        RebuildPolicy {
            max_attempts,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(4),
        }
    }

    /// Zero replicas is a configuration mistake worth naming, not a value to
    /// silently coerce to one. Rejected before any I/O, so this needs no model
    /// file on disk.
    #[tokio::test]
    async fn zero_replicas_fails_to_load_naming_model_and_field() {
        let cfg = ModelConfig {
            model_id: "zero-replica-model".to_owned(),
            model_dir: PathBuf::from("/nonexistent"),
            checksum: Checksum::Skip,
            replicas: 0,
            ..ModelConfig::default()
        };

        let Err(LoadError::Failed(err)) = LoadedModel::load(&cfg).await else {
            panic!("zero replicas must not load");
        };
        assert_eq!(err.reason, ModelFailureReason::InvalidConfig);
        let message = err.to_string();
        assert!(message.contains("zero-replica-model"), "{message}");
        assert!(message.contains("`replicas`"), "{message}");
    }

    // --- Labels --------------------------------------------------------------

    #[test]
    fn labels_parse_into_class_id_order() {
        let raw = r#"{"id2label": {"2": "B-EMAIL", "0": "O", "1": "B-PER"}}"#;
        assert_eq!(
            parse_labels(raw, "m"),
            Some(vec![
                "O".to_owned(),
                "B-PER".to_owned(),
                "B-EMAIL".to_owned()
            ])
        );
    }

    #[test]
    fn labels_that_cannot_be_trusted_positionally_are_dropped() {
        for raw in [
            "not json",
            r#"{"other": {}}"#,
            r#"{"id2label": []}"#,
            // A gap would shift every later label.
            r#"{"id2label": {"0": "O", "2": "B-PER"}}"#,
            // Not starting at zero.
            r#"{"id2label": {"1": "O"}}"#,
            // A non-numeric key.
            r#"{"id2label": {"0": "O", "x": "B-PER"}}"#,
            // A non-string label.
            r#"{"id2label": {"0": 7}}"#,
        ] {
            assert_eq!(parse_labels(raw, "m"), None, "{raw}");
        }
        assert_eq!(parse_labels(r#"{"id2label": {}}"#, "m"), Some(Vec::new()));
    }

    #[test]
    fn an_absent_labels_file_is_none() {
        assert_eq!(
            read_labels(Path::new("/nonexistent/config.json"), "m"),
            None
        );
    }

    // --- Integrity -----------------------------------------------------------

    #[test]
    fn streamed_hash_equals_the_in_memory_hash_across_chunks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("model.onnx");
        // Over two chunks and not a multiple of one, so a boundary and a
        // short final read are both crossed.
        let bytes: Vec<u8> = (0..HASH_CHUNK_BYTES * 2 + 17)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        std::fs::write(&path, &bytes).expect("writes");
        assert_eq!(hash_file(&path).expect("hashes"), sha256_of(&bytes));
    }

    fn model_in(dir: &Path, checksum: Checksum) -> ModelConfig {
        ModelConfig {
            model_id: "m".to_owned(),
            model_dir: dir.to_owned(),
            checksum,
            ..ModelConfig::default()
        }
    }

    #[tokio::test]
    async fn model_file_checks_classify_each_outcome() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("model.onnx"), b"weights").expect("writes");
        let real = sha256_of(b"weights");

        assert_eq!(
            check_model_file(&model_in(dir.path(), Checksum::Sha256(real)))
                .await
                .expect("matches"),
            Some(real)
        );
        assert_eq!(
            check_model_file(&model_in(dir.path(), Checksum::Skip))
                .await
                .expect("readable"),
            None
        );

        let wrong = Sha256Digest::from_bytes([1; 32]);
        let err = check_model_file(&model_in(dir.path(), Checksum::Sha256(wrong)))
            .await
            .expect_err("mismatch");
        assert_eq!(err.reason, ModelFailureReason::ChecksumMismatch);
        assert!(
            matches!(*err.source, LoadFailureSource::ChecksumMismatch { expected, actual, .. }
                if expected == wrong && actual == real),
            "{err}"
        );

        let empty = tempfile::tempdir().expect("tempdir");
        for checksum in [Checksum::Sha256(real), Checksum::Skip] {
            let err = check_model_file(&model_in(empty.path(), checksum))
                .await
                .expect_err("missing");
            assert_eq!(err.reason, ModelFailureReason::FileUnreadable);
            assert!(matches!(*err.source, LoadFailureSource::Io { .. }), "{err}");
            assert!(
                std::error::Error::source(&err).is_some(),
                "typed source kept"
            );
        }
    }

    // --- Rebuild supervision -------------------------------------------------

    #[tokio::test]
    async fn a_rebuild_retries_with_backoff_until_it_succeeds() {
        let calls = AtomicUsize::new(0);
        let outcome = rebuild_with_backoff(
            "m",
            fast_policy(5),
            &CancellationToken::new(),
            &AtomicBool::new(false),
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { if n < 2 { Err(failure()) } else { Ok(42_u8) } }
            },
        )
        .await;
        assert!(
            matches!(outcome, RebuildOutcome::Rebuilt(42)),
            "{outcome:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_rebuild_gives_up_after_its_attempts() {
        let calls = AtomicUsize::new(0);
        let outcome: RebuildOutcome<u8> = rebuild_with_backoff(
            "m",
            fast_policy(3),
            &CancellationToken::new(),
            &AtomicBool::new(false),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err(failure()) }
            },
        )
        .await;
        assert!(
            matches!(outcome, RebuildOutcome::GaveUp { attempts: 3 }),
            "{outcome:?}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "exactly max_attempts opens"
        );
    }

    /// A hang leaks a thread per attempt, so it is never retried — and once
    /// one replica has seen it, no other replica of the model tries at all.
    #[tokio::test]
    async fn a_hung_runtime_is_terminal_and_shared() {
        let hung = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let open = || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err::<u8, _>(LoadError::RuntimeHung { seconds: 60 }) }
        };
        let first =
            rebuild_with_backoff("m", fast_policy(5), &CancellationToken::new(), &hung, open).await;
        assert!(matches!(first, RebuildOutcome::RuntimeHung), "{first:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(hung.load(Ordering::SeqCst));

        let second =
            rebuild_with_backoff("m", fast_policy(5), &CancellationToken::new(), &hung, open).await;
        assert!(matches!(second, RebuildOutcome::RuntimeHung), "{second:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no further open attempted");
    }

    #[tokio::test]
    async fn shutdown_ends_a_rebuild_during_its_backoff() {
        let shutdown = CancellationToken::new();
        let policy = RebuildPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_secs(3600),
            max_backoff: Duration::from_secs(3600),
        };
        let canceller = shutdown.clone();
        let outcome: RebuildOutcome<u8> = tokio::time::timeout(
            Duration::from_secs(5),
            rebuild_with_backoff("m", policy, &shutdown, &AtomicBool::new(false), || {
                canceller.cancel();
                async { Err(failure()) }
            }),
        )
        .await
        .expect("does not wait out the backoff");
        assert!(matches!(outcome, RebuildOutcome::Cancelled), "{outcome:?}");
    }

    #[derive(Default)]
    struct Rebuilds(StdMutex<Vec<bool>>);

    impl RuntimeMetricsPort for Rebuilds {
        fn record_inference(&self, _: &str, _: InferenceOutcome, _: InferenceTiming) {}
        fn record_tokenize(&self, _: &str, _: InferenceOutcome, _: Duration) {}
        fn record_in_flight_delta(&self, _: &str, _: i64) {}
        fn record_model_load(&self, _: &str, _: LoadOutcome, _: Duration, _: u64) {}
        fn record_session_rebuild(&self, _: &str, succeeded: bool) {
            self.0.lock().expect("not poisoned").push(succeeded);
        }
    }

    /// Two replicas; one is emptied and handed to a supervisor, as after a
    /// panic. Returns what `settle_rebuild` needs.
    async fn emptied_replica(
        pool: &Arc<ReplicaPool>,
        health: &ReplicaHealth,
    ) -> (OwnedMutexGuard<Option<u8>>, ReplicaLease) {
        let lease = pool.acquire().await.expect("leases");
        let mut guard = Arc::new(Mutex::new(Some(1_u8))).lock_owned().await;
        guard.take();
        health.rebuilding.fetch_add(1, Ordering::AcqRel);
        (guard, lease)
    }

    #[tokio::test]
    async fn a_successful_rebuild_restores_the_replica_to_the_pool() {
        let pool = ReplicaPool::new(NonZeroUsize::MIN);
        let health = ReplicaHealth::new(1);
        let metrics = Rebuilds::default();
        let (guard, lease) = emptied_replica(&pool, &health).await;
        assert_eq!(health.live(), 0, "a rebuilding replica is not live");

        let slot = Arc::clone(OwnedMutexGuard::mutex(&guard));
        settle_rebuild(
            "m",
            &health,
            &pool,
            &metrics,
            guard,
            lease,
            RebuildOutcome::Rebuilt(9),
        );

        assert_eq!(*slot.lock().await, Some(9));
        assert_eq!(health.live(), 1);
        assert!(!health.is_unavailable());
        assert!(pool.acquire().await.is_some(), "leasable again");
        assert_eq!(*metrics.0.lock().expect("not poisoned"), vec![true]);
    }

    /// The transition the old code got wrong: a failed rebuild of the last
    /// replica makes the model unavailable, and waiters stop waiting.
    #[tokio::test]
    async fn a_failed_rebuild_retires_the_replica_and_the_last_one_closes_the_model() {
        let pool = ReplicaPool::new(NonZeroUsize::new(2).expect("non-zero"));
        let health = ReplicaHealth::new(2);
        let metrics = Rebuilds::default();

        let (guard, lease) = emptied_replica(&pool, &health).await;
        settle_rebuild(
            "m",
            &health,
            &pool,
            &metrics,
            guard,
            lease,
            RebuildOutcome::GaveUp { attempts: 5 },
        );
        assert_eq!(health.live(), 1, "one replica still serves");
        assert!(!health.is_unavailable());

        let (guard, lease) = emptied_replica(&pool, &health).await;
        settle_rebuild(
            "m",
            &health,
            &pool,
            &metrics,
            guard,
            lease,
            RebuildOutcome::RuntimeHung,
        );
        assert_eq!(health.live(), 0);
        assert!(health.is_unavailable(), "no replica left");
        assert!(pool.acquire().await.is_none(), "the pool is closed");
        assert_eq!(*metrics.0.lock().expect("not poisoned"), vec![false, false]);
    }

    #[tokio::test]
    async fn a_cancelled_rebuild_retires_without_counting_a_failure() {
        let pool = ReplicaPool::new(NonZeroUsize::new(2).expect("non-zero"));
        let health = ReplicaHealth::new(2);
        let metrics = Rebuilds::default();
        let (guard, lease) = emptied_replica(&pool, &health).await;
        settle_rebuild(
            "m",
            &health,
            &pool,
            &metrics,
            guard,
            lease,
            RebuildOutcome::Cancelled,
        );
        assert_eq!(health.live(), 1);
        assert!(metrics.0.lock().expect("not poisoned").is_empty());
    }

    // --- Load shedding -------------------------------------------------------

    fn limit(max: usize, policy: OverloadPolicy, queue_timeout: Duration) -> InFlightLimit {
        InFlightLimit::new(
            NonZeroUsize::new(max).expect("non-zero"),
            policy,
            queue_timeout,
        )
    }

    #[tokio::test]
    async fn reject_sheds_at_the_configured_ceiling() {
        let limit = limit(2, OverloadPolicy::Reject, Duration::from_secs(1));
        let a = limit.acquire("m").await.expect("first");
        let b = limit.acquire("m").await.expect("second");
        let err = limit.acquire("m").await.expect_err("third is shed");
        assert!(
            matches!(
                err,
                RuntimeError::Overloaded {
                    max_in_flight: 2,
                    ..
                }
            ),
            "reports the ceiling, not the replica count: {err}"
        );
        drop((a, b));
        assert!(limit.acquire("m").await.is_ok());
    }

    #[tokio::test]
    async fn queue_waits_then_times_out_as_overloaded() {
        let limit = limit(1, OverloadPolicy::Queue, Duration::from_millis(20));
        let held = limit.acquire("m").await.expect("first");
        let started = Instant::now();
        let err = limit.acquire("m").await.expect_err("times out");
        assert!(
            started.elapsed() >= Duration::from_millis(20),
            "it queued first"
        );
        assert!(
            matches!(
                err,
                RuntimeError::Overloaded {
                    max_in_flight: 1,
                    ..
                }
            ),
            "{err}"
        );
        drop(held);
    }

    #[tokio::test]
    async fn queue_hands_over_a_released_permit() {
        let limit = Arc::new(limit(1, OverloadPolicy::Queue, Duration::from_secs(5)));
        let held = limit.acquire("m").await.expect("first");
        let waiter = {
            let limit = Arc::clone(&limit);
            tokio::spawn(async move { limit.acquire("m").await.is_ok() })
        };
        tokio::task::yield_now().await;
        drop(held);
        assert!(
            waiter.await.expect("joins"),
            "the queued caller gets the permit"
        );
    }

    #[test]
    fn panic_payloads_are_read_as_text() {
        let from_str: Box<dyn std::any::Any + Send> = Box::new("boom");
        let from_string: Box<dyn std::any::Any + Send> = Box::new(String::from("bang"));
        let other: Box<dyn std::any::Any + Send> = Box::new(7_u8);
        assert_eq!(panic_message(&*from_str), "boom");
        assert_eq!(panic_message(&*from_string), "bang");
        assert_eq!(panic_message(&*other), "<non-string panic payload>");
    }
}

/// Concurrency, panic-recovery and cancel-safety tests, which need a session
/// and therefore a real ONNX model on disk. Enabled with
/// `--features real-model-tests`. The committed fixture graph that lets the
/// rest of the suite run without one is described in
/// `tests/fixtures/README.md`.
#[cfg(all(test, feature = "real-model-tests"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[expect(
    clippy::expect_used,
    reason = "a missing model on disk should fail the helper loudly"
)]
mod real_model_tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use super::LoadedModel;
    use crate::runtime::config::{Checksum, ModelConfig, OutputDtype};

    /// A model fetched by `scripts/fetch-models.sh`. The graph is never run
    /// here — these tests drive `with_replica` directly, so only loading has
    /// to succeed.
    fn test_model(replicas: usize) -> ModelConfig {
        let model_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("models/gliner2-pii")
            .canonicalize()
            .expect("run scripts/fetch-models.sh first");
        ModelConfig {
            model_id: "test-model".to_owned(),
            model_dir,
            model_file: "model.quantized.onnx".to_owned(),
            checksum: Checksum::Skip,
            expected_inputs: vec![
                "input_ids".to_owned(),
                "attention_mask".to_owned(),
                "text_word_indices".to_owned(),
                "schema_special_indices".to_owned(),
            ],
            expected_outputs: vec!["span_probs".to_owned(), "count_logits".to_owned()],
            output_dtypes: [
                ("span_probs".to_owned(), OutputDtype::F32),
                ("count_logits".to_owned(), OutputDtype::F32),
            ]
            .into_iter()
            .collect(),
            replicas,
            intra_threads: 1,
            ..ModelConfig::default()
        }
    }

    /// Two replicas, a two-worker runtime, and two inferences that each occupy
    /// a thread for 300 ms. Run in parallel they finish in about 300 ms; run
    /// serially — or inline on the workers, starving the timer — they take
    /// at least 600 ms.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_calls_do_not_starve_the_runtime() {
        let model = Arc::new(
            LoadedModel::load(&test_model(2))
                .await
                .expect("model loads"),
        );

        let started = Instant::now();
        let calls: Vec<_> = (0..2)
            .map(|_| {
                let model = Arc::clone(&model);
                tokio::spawn(async move {
                    model
                        .with_replica(|_session| {
                            std::thread::sleep(Duration::from_millis(300));
                            Ok(())
                        })
                        .await
                })
            })
            .collect();

        let results = tokio::time::timeout(Duration::from_secs(5), async {
            let mut out = Vec::new();
            for call in calls {
                out.push(call.await.expect("task joins"));
            }
            out
        })
        .await
        .expect("both inferences complete without hanging the runtime");
        let elapsed = started.elapsed();

        assert!(results.iter().all(Result::is_ok), "both calls succeed");
        assert!(
            elapsed < Duration::from_millis(590),
            "two replicas must run two calls in parallel; took {elapsed:?}"
        );
    }

    /// An unrelated task must keep making progress while inference occupies a
    /// blocking thread — the property that makes this gear safe to deploy
    /// beside others in one process.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unrelated_tasks_progress_during_inference() {
        let model = Arc::new(
            LoadedModel::load(&test_model(1))
                .await
                .expect("model loads"),
        );
        let ticks = Arc::new(AtomicUsize::new(0));

        let ticker = {
            let ticks = Arc::clone(&ticks);
            tokio::spawn(async move {
                for _ in 0..10 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    ticks.fetch_add(1, Ordering::Relaxed);
                }
            })
        };

        model
            .with_replica(|_session| {
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            })
            .await
            .expect("inference completes");

        assert!(
            ticks.load(Ordering::Relaxed) > 0,
            "an unrelated task made no progress while inference was in flight"
        );
        ticker.await.expect("ticker joins");
    }

    /// A panicking inference costs one session, not the gear: the call returns
    /// a typed error rather than unwinding into the caller, the replica goes to
    /// a supervisor, and the next call is served by the rebuilt session.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn panic_costs_one_session_and_the_next_call_recovers() {
        let model = LoadedModel::load(&test_model(1))
            .await
            .expect("model loads");

        let panicked = model
            .with_replica(|_session| -> Result<(), _> { panic!("simulated inference panic") })
            .await;
        assert!(panicked.is_err(), "a panicking inference returns an error");

        let recovered = tokio::time::timeout(
            Duration::from_secs(120),
            model.with_replica(|_session| Ok(())),
        )
        .await
        .expect("the supervisor rebuilds the replica");
        assert!(
            recovered.is_ok(),
            "the next call must be served by the rebuilt session: {recovered:?}"
        );
        assert_eq!(model.live_replicas(), 1);
    }

    /// Cancelling a call must not strand a replica. The in-flight work runs to
    /// completion and the session returns to its slot, so the pool is intact
    /// afterwards.
    ///
    /// The cancellation has to happen *mid-flight* to prove anything: an async
    /// fn's future does nothing until first polled, so dropping one before
    /// awaiting it would exercise no code at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_call_leaves_the_replica_usable() {
        let model = LoadedModel::load(&test_model(1))
            .await
            .expect("model loads");

        for _ in 0..20 {
            let cancelled = tokio::time::timeout(
                Duration::from_millis(20),
                model.with_replica(|_session| {
                    std::thread::sleep(Duration::from_millis(200));
                    Ok(())
                }),
            )
            .await;
            assert!(
                cancelled.is_err(),
                "the call must be cancelled while in flight"
            );
        }

        let after = tokio::time::timeout(
            Duration::from_secs(30),
            model.with_replica(|_session| Ok(())),
        )
        .await
        .expect("a replica becomes available again after cancelled calls");
        assert!(
            after.is_ok(),
            "replica usable after cancellation: {after:?}"
        );
    }
}
