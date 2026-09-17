//! The in-process model host: loads every configured model once, verifies it,
//! and serves tensor execution and tokenization to the detector backends.
//!
//! This is not a gear and exposes no HTTP API (see
//! `cpt-cf-onnx-runner-constraint-no-http-surface` in `docs/runtime/DESIGN.md`
//! §3.5). `onnx-scanner` owns it and builds it at `init`; readiness reaches
//! the platform through the `Healthcheck` registry ([`super::health`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use super::{
    ModelFailureReason, ModelInfo, ModelRuntime, NamedTensor, RuntimeError, TokenEncoding,
};
use async_trait::async_trait;
use tracing::{error, info, warn};

use super::config::{Checksum, ConfigError, ModelConfig, validate_model_ids};
use super::metrics_port::{LoadOutcome, RuntimeMetricsPort};
use super::session::{LoadError, LoadFailure, LoadedModel, check_model_file, read_labels};
use super::tokenizer::LoadedTokenizer;

/// One configured model: either a loaded session pool with its tokenizer, or
/// the classified reason it is not usable.
///
/// Both cases are kept, which is the whole point of this type — a model that
/// failed to load is *unavailable*, not absent, and a consumer must be able to
/// tell that from a typo'd id.
pub enum ModelEntry {
    Ready(Box<LoadedEntry>),
    Unavailable(ModelFailureReason),
}

impl ModelEntry {
    const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }
}

/// A loaded model's session pool plus its tokenizer.
pub struct LoadedEntry {
    model: LoadedModel,
    tokenizer: LoadedTokenizer,
}

/// Why model loading failed, with the model that failed named.
///
/// Only reachable when `require_all_models` is set: with the permissive
/// default a per-model failure becomes [`ModelEntry::Unavailable`] instead.
#[derive(Debug, thiserror::Error)]
#[error("model {model_id}: {source}")]
pub struct ModelLoadError {
    pub model_id: String,
    #[source]
    pub source: LoadFailure,
}

/// Why the model host failed to start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InitError {
    /// The model list itself is unusable: a malformed or repeated model id.
    /// Fatal whatever `require_all_models` says, because ids are map keys and
    /// appear on `/health`.
    #[error("invalid model configuration: {0}")]
    InvalidConfig(#[from] ConfigError),
    /// `require_all_models` is set and a model could not be loaded.
    #[error(transparent)]
    Model(#[from] ModelLoadError),
    /// ONNX Runtime did not load in time (a bad `ORT_DYLIB_PATH`). The thread
    /// that tried was abandoned, so the process must terminate: see
    /// [`InitError::leaked_init_thread`].
    #[error(
        "ONNX Runtime did not load within {seconds}s (check ORT_DYLIB_PATH); an init thread was \
         abandoned and the process must terminate"
    )]
    RuntimeHung { seconds: u64 },
    /// Shutdown was requested while models were loading. `open_in_flight`
    /// records whether a session open was under way: its thread cannot be
    /// stopped and is abandoned.
    #[error("startup cancelled while models were loading")]
    Cancelled { open_in_flight: bool },
}

impl InitError {
    /// Whether a thread was left running that cannot be stopped. The caller
    /// must then terminate the process rather than retry.
    #[must_use]
    pub const fn leaked_init_thread(&self) -> bool {
        matches!(
            self,
            Self::RuntimeHung { .. }
                | Self::Cancelled {
                    open_in_flight: true
                }
        )
    }
}

/// Loads every configured model and tokenizer.
///
/// A model that fails to load is recorded [`ModelEntry::Unavailable`] and the
/// host still starts — one bad artifact must not take down every other gear in
/// the process. `require_all_models` restores fail-fast for deployments that
/// prefer it. Malformed or repeated model ids fail startup either way.
///
/// The ONNX Runtime hang on an unloadable `ORT_DYLIB_PATH` is the toolkit's to
/// guard against (`toolkit_onnx_runtime::OnnxSession::open` opens under a
/// deadline on an abandonable thread); this function only ends the wait early
/// when shutdown is requested, so a wedged load never holds up graceful
/// shutdown for the full budget.
///
/// # Errors
///
/// See [`InitError`] — [`InitError::leaked_init_thread`] obliges the caller to
/// terminate the process.
pub async fn load_models(
    models: &[ModelConfig],
    require_all_models: bool,
    metrics: Arc<dyn RuntimeMetricsPort>,
    cancellation_token: &tokio_util::sync::CancellationToken,
) -> Result<BTreeMap<String, ModelEntry>, InitError> {
    validate_model_ids(models)?;
    // Set while a session open is in flight, so a cancellation can say
    // whether it abandoned one.
    let opening = AtomicBool::new(false);
    tokio::select! {
        loaded = load_models_inner(models, require_all_models, &metrics, &opening) => loaded,
        () = cancellation_token.cancelled() => Err(InitError::Cancelled {
            open_in_flight: opening.load(Ordering::Acquire),
        }),
    }
}

async fn load_models_inner(
    models: &[ModelConfig],
    require_all_models: bool,
    metrics: &Arc<dyn RuntimeMetricsPort>,
    opening: &AtomicBool,
) -> Result<BTreeMap<String, ModelEntry>, InitError> {
    let mut loaded = BTreeMap::new();

    for model_cfg in models {
        let started = Instant::now();
        let entry = match load_one(model_cfg, metrics, opening).await {
            Ok(entry) => {
                // `replicas × file size` — a floor, not a measurement: it
                // excludes ONNX Runtime's arenas and per-session thread
                // stacks. Enough to make an oversized config visible in the
                // startup log without reaching for a profiler.
                let resident = resident_bytes_estimate(model_cfg).await;
                metrics.record_model_load(
                    &model_cfg.model_id,
                    LoadOutcome::Ok,
                    started.elapsed(),
                    resident,
                );
                info!(
                    model_id = %model_cfg.model_id,
                    replicas = model_cfg.replicas,
                    resident_bytes = resident,
                    elapsed_ms = started.elapsed().as_millis(),
                    "onnx-scanner: loaded model"
                );
                ModelEntry::Ready(Box::new(entry))
            }
            Err(LoadError::RuntimeHung { seconds }) => {
                return Err(InitError::RuntimeHung { seconds });
            }
            Err(LoadError::Failed(failure)) => {
                metrics.record_model_load(
                    &model_cfg.model_id,
                    LoadOutcome::Failed(failure.reason),
                    started.elapsed(),
                    0,
                );
                if require_all_models {
                    return Err(ModelLoadError {
                        model_id: model_cfg.model_id.clone(),
                        source: failure,
                    }
                    .into());
                }
                degraded_entry(&model_cfg.model_id, &failure)
            }
        };
        loaded.insert(model_cfg.model_id.clone(), entry);
    }

    if !models.is_empty() && !loaded.values().any(ModelEntry::is_ready) {
        // Not a startup failure under the permissive default — but a host
        // serving nothing is almost never what the operator intended, and it
        // should not be something they discover from a consumer's coverage
        // report.
        error!(
            configured = models.len(),
            "onnx-scanner: no model loaded successfully; every inference request will fail"
        );
    }

    Ok(loaded)
}

/// Logs a call rejected before it reached a session.
///
/// At `debug`, not `warn`: an unknown model id or an oversized input is a
/// caller-side mistake, and a misbehaving consumer must not be able to fill an
/// operator's logs. The metrics counters are where the *rate* of these belongs.
///
/// Only the error is logged. It carries model ids, tensor names and sizes —
/// never payload content — which is what
/// `cpt-cf-onnx-runner-principle-no-payload-in-telemetry` requires and what
/// `tests/telemetry_sentinel_tests.rs` enforces.
fn log_dispatch_failure(error: &RuntimeError) {
    tracing::debug!(%error, "onnx-scanner: call rejected before execution");
}

/// Records a model as unavailable, logging the failure once — here, where
/// the classification is known.
///
/// `reason` is the stable code a consumer branches on; `error` carries the
/// detail an operator needs and never leaves this process.
fn degraded_entry(model_id: &str, failure: &LoadFailure) -> ModelEntry {
    warn!(
        model_id = %model_id,
        reason = failure.reason.as_code(),
        error = %failure.source,
        "onnx-scanner: model unavailable; the gear will start without it"
    );
    ModelEntry::Unavailable(failure.reason)
}

/// Loads one model and its tokenizer, classifying whichever step fails.
///
/// Ordered cheapest-first and runtime-last: the config, the model file's
/// digest, the tokenizer and labels are all checked before any ONNX Runtime
/// session is opened, so every failure that does not need the runtime is
/// classified without it. File I/O runs on the blocking pool.
async fn load_one(
    cfg: &ModelConfig,
    metrics: &Arc<dyn RuntimeMetricsPort>,
    opening: &AtomicBool,
) -> Result<LoadedEntry, LoadError> {
    cfg.validate()
        .map_err(|e| LoadFailure::new(ModelFailureReason::InvalidConfig, e))?;
    let verified = check_model_file(cfg).await?;

    if matches!(cfg.checksum, Checksum::Sha256(_)) && cfg.tokenizer_sha256.is_none() {
        warn!(
            model_id = %cfg.model_id,
            "onnx-scanner: the model is pinned but its tokenizer is not; set `tokenizer_sha256`"
        );
    }
    let tokenizer_path = cfg.model_dir.join(&cfg.tokenizer_file);
    let labels_path = cfg.model_dir.join(&cfg.labels_file);
    let expected_tokenizer = cfg.tokenizer_sha256;
    let model_id = cfg.model_id.clone();
    let (tokenizer, labels) = tokio::task::spawn_blocking(move || {
        (
            LoadedTokenizer::load_verified(&tokenizer_path, expected_tokenizer),
            read_labels(&labels_path, &model_id),
        )
    })
    .await
    .map_err(|e| LoadFailure::new(ModelFailureReason::TokenizerInvalid, e))?;
    let tokenizer = tokenizer
        .map_err(|e| LoadFailure::new(ModelFailureReason::TokenizerInvalid, e))?
        .with_metrics(Arc::clone(metrics));

    opening.store(true, Ordering::Release);
    let model = LoadedModel::open(cfg, verified, labels, Arc::clone(metrics)).await;
    opening.store(false, Ordering::Release);
    Ok(LoadedEntry {
        model: model?,
        tokenizer,
    })
}

/// Estimated resident footprint for a loaded model.
///
/// `replicas × model file size`. A floor rather than a measurement — ONNX
/// Runtime's arenas, per-session thread stacks and pre-packed weights are not
/// counted — but it is the number that makes "4 replicas of a 446 MB model"
/// visible at startup instead of in an out-of-memory kill.
async fn resident_bytes_estimate(cfg: &ModelConfig) -> u64 {
    let path = cfg.model_dir.join(&cfg.model_file);
    let file_size =
        tokio::task::spawn_blocking(move || std::fs::metadata(path).map_or(0, |m| m.len()))
            .await
            .unwrap_or(0);
    file_size.saturating_mul(u64::try_from(cfg.replicas).unwrap_or(u64::MAX))
}

/// In-process implementation of [`ModelRuntime`], built by `onnx-scanner` at
/// `init`.
pub struct LocalRuntime {
    /// Every *configured* model, not only the ones that loaded.
    models: BTreeMap<String, ModelEntry>,
}

impl LocalRuntime {
    /// Builds the runtime over an already-loaded model map.
    #[must_use]
    pub const fn new(models: BTreeMap<String, ModelEntry>) -> Self {
        Self { models }
    }

    /// Rejects oversized text before tokenizing it.
    ///
    /// Rejection rather than truncation is deliberate: silently shortening a
    /// caller's text would make the offsets it gets back describe a document
    /// that was never scanned. A truncation *policy* belongs to the tokenizer
    /// feature that owns padding and windowing, not to a size guard.
    fn check_text(entry: &LoadedEntry, model_id: &str, text: &str) -> Result<(), RuntimeError> {
        let limit = entry.model.limits().text_bytes;
        if text.len() > limit {
            return Err(RuntimeError::InvalidInput {
                model_id: model_id.to_owned(),
                tensor: "<text>".to_owned(),
                message: format!("{} bytes of text, over the limit of {limit}", text.len()),
            });
        }
        Ok(())
    }

    /// Resolves a model id to a usable entry, keeping the two failure modes
    /// apart: not configured at all (a caller mistake) versus configured but
    /// unusable (an outage the consumer must report as degraded coverage).
    fn ready_entry(&self, model_id: &str) -> Result<&LoadedEntry, RuntimeError> {
        match self.models.get(model_id) {
            Some(ModelEntry::Ready(entry)) => Ok(entry),
            Some(ModelEntry::Unavailable(reason)) => Err(RuntimeError::ModelUnavailable {
                model_id: model_id.to_owned(),
                reason: *reason,
            }),
            None => Err(RuntimeError::UnknownModel {
                model_id: model_id.to_owned(),
            }),
        }
    }
}

#[async_trait]
impl ModelRuntime for LocalRuntime {
    #[tracing::instrument(
        name = "onnx_scanner.runtime.run",
        skip_all,
        fields(model_id = %model_id, tensors = inputs.len())
    )]
    async fn run(
        &self,
        model_id: &str,
        inputs: Vec<NamedTensor>,
    ) -> Result<Vec<NamedTensor>, RuntimeError> {
        // The span exists at this layer, not only in `infra`, because a call
        // rejected here — unknown model, unavailable model — never reaches a
        // session and would otherwise leave no trace at all.
        self.ready_entry(model_id)
            .inspect_err(log_dispatch_failure)?
            .model
            .run(inputs)
            .await
    }

    #[tracing::instrument(
        name = "onnx_scanner.runtime.encode",
        skip_all,
        fields(model_id = %model_id, text_bytes = text.len())
    )]
    async fn encode(&self, model_id: &str, text: &str) -> Result<TokenEncoding, RuntimeError> {
        let entry = self
            .ready_entry(model_id)
            .inspect_err(log_dispatch_failure)?;
        Self::check_text(entry, model_id, text).inspect_err(log_dispatch_failure)?;
        entry.tokenizer.encode(model_id, text.to_owned()).await
    }

    #[tracing::instrument(
        name = "onnx_scanner.runtime.encode_fragment",
        skip_all,
        fields(model_id = %model_id, text_bytes = fragment.len())
    )]
    async fn encode_fragment(
        &self,
        model_id: &str,
        fragment: &str,
    ) -> Result<Vec<i64>, RuntimeError> {
        let entry = self
            .ready_entry(model_id)
            .inspect_err(log_dispatch_failure)?;
        Self::check_text(entry, model_id, fragment).inspect_err(log_dispatch_failure)?;
        entry
            .tokenizer
            .encode_fragment(model_id, fragment.to_owned())
            .await
    }

    #[tracing::instrument(
        name = "onnx_scanner.runtime.encode_fragments",
        skip_all,
        fields(model_id = %model_id, fragments = fragments.len())
    )]
    async fn encode_fragments(
        &self,
        model_id: &str,
        fragments: &[&str],
    ) -> Result<Vec<Vec<i64>>, RuntimeError> {
        let entry = self
            .ready_entry(model_id)
            .inspect_err(log_dispatch_failure)?;
        // Bounded as one text: the batch is one blocking task, so its total
        // size is what the per-call limit has to cap.
        let total = fragments
            .iter()
            .map(|f| f.len())
            .fold(0_usize, usize::saturating_add);
        let limit = entry.model.limits().text_bytes;
        if total > limit {
            let err = RuntimeError::InvalidInput {
                model_id: model_id.to_owned(),
                tensor: "<text>".to_owned(),
                message: format!("{total} bytes of fragments, over the limit of {limit}"),
            };
            log_dispatch_failure(&err);
            return Err(err);
        }
        let owned = fragments.iter().map(|f| (*f).to_owned()).collect();
        entry.tokenizer.encode_fragments(model_id, owned).await
    }

    /// Reports every configured model, including the ones that failed to load.
    /// A caller that finds no entry is looking at a typo; one that finds
    /// `Unavailable` is looking at an outage.
    async fn list_models(&self) -> Result<Vec<ModelInfo>, RuntimeError> {
        Ok(self.model_snapshot())
    }
}

impl LocalRuntime {
    /// Holds one of `model_id`'s replicas until the returned value is dropped.
    /// Test support; see [`super::session::ReplicaHold`].
    ///
    /// # Errors
    ///
    /// The model is unknown or unavailable.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub async fn hold_replica(
        &self,
        model_id: &str,
    ) -> Result<super::session::ReplicaHold, RuntimeError> {
        let entry = self.ready_entry(model_id)?;
        entry
            .model
            .hold_replica()
            .await
            .ok_or_else(|| RuntimeError::ModelUnavailable {
                model_id: model_id.to_owned(),
                reason: ModelFailureReason::SessionBuildFailed,
            })
    }

    /// Every configured model's current state, read now rather than at load.
    ///
    /// Shared by [`ModelRuntime::list_models`] and the platform
    /// healthcheck so operators and consumers cannot be told different
    /// stories about the same model. Cheap by construction: it clones
    /// recorded metadata and touches no session.
    #[must_use]
    pub fn model_snapshot(&self) -> Vec<ModelInfo> {
        self.models
            .iter()
            .map(|(model_id, entry)| match entry {
                // Read now rather than frozen at load: replicas are lost to
                // panicking inferences and rebuilt in the background, and a
                // model that loses all of them becomes unavailable.
                ModelEntry::Ready(loaded) => loaded.model.snapshot(),
                ModelEntry::Unavailable(reason) => {
                    ModelInfo::unavailable(model_id.clone(), *reason)
                }
            })
            .collect()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn only_a_hang_or_a_cancelled_open_leaks_a_thread() {
        let cases = [
            (InitError::RuntimeHung { seconds: 60 }, true),
            (
                InitError::Cancelled {
                    open_in_flight: true,
                },
                true,
            ),
            (
                InitError::Cancelled {
                    open_in_flight: false,
                },
                false,
            ),
            (
                InitError::InvalidConfig(ConfigError::DuplicateModelId {
                    model_id: "m".to_owned(),
                }),
                false,
            ),
        ];
        for (err, leaked) in cases {
            assert_eq!(err.leaked_init_thread(), leaked, "{err}");
        }
    }
}
