//! ONNX-model-backed PII scanner backend plugin gear.

use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use async_trait::async_trait;
use prompt_scanner_sdk::{ScannerBackendClient, ScannerBackendSpecV1};
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::contracts::OpenApiRegistry;
use toolkit::gts::PluginV1;
use toolkit::{Gear, Healthcheck, RestApiCapability};
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::{OnnxScannerConfig, PiiBackendConfig};
use crate::domain::{BackendSpec, InputLimits, Service, build_backend};
use crate::runtime::health::ModelReadiness;
use crate::runtime::metrics_port::RuntimeMetricsPort;
use crate::runtime::{LocalRuntime, ModelRuntime, RuntimeMetricsMeter, load_models};

/// Everything `init` builds, published in one step once `init` has fully
/// succeeded — a half-initialized gear never reports health or serves.
/// The service itself is owned by its `ClientHub` registration.
struct GearState {
    runtime: Arc<LocalRuntime>,
}

/// ONNX-model-backed PII detection `ScannerBackend` plugin. Replaces
/// `pii-plugin` (retired) and the standalone `onnx-runner` gear — it loads and
/// runs its models itself (see `crate::runtime`).
/// Prompt-injection detection is out of scope for now; the Python
/// `GLiNER2`-sidecar-backed `prompt-injection-plugin` was removed entirely
/// rather than kept as an external dependency.
///
/// **Plugin registration pattern:**
/// - The `prompt-scanner` gear owns the `ScannerBackendSpecV1` GTS schema
///   (published automatically via link-time inventory — no per-init
///   registration needed on the gateway side).
/// - This plugin registers its instance (implementation metadata) and its
///   scoped client (`ClientHub` entry) so `prompt-scanner`'s fan-out
///   resolution logic can discover and call it.
// `rest` is declared for the `healthcheck` hook alone: `register_rest` adds no
// routes (see the `RestApiCapability` impl below).
#[toolkit::gear(name = "onnx-scanner", deps = [types_registry], capabilities = [rest])]
#[derive(Default)]
pub struct OnnxScanner {
    state: OnceLock<GearState>,
}

/// The backend spec the config selects. `max_text_bytes` comes from the
/// backend model's own entry, which `validate` guarantees exists.
fn backend_spec(cfg: &OnnxScannerConfig) -> anyhow::Result<BackendSpec> {
    let model = cfg
        .backend_model()
        .context("onnx-scanner: backend model has no `models` entry")?;
    Ok(match &cfg.backend {
        PiiBackendConfig::BioTokenClassifier { model_id, labels } => BackendSpec::Bio {
            model_id: model_id.clone(),
            labels: labels.clone(),
        },
        PiiBackendConfig::Gliner2 {
            model_id,
            labels,
            decode_threshold,
        } => BackendSpec::Gliner2 {
            model_id: model_id.clone(),
            labels: labels.clone(),
            decode_threshold: *decode_threshold,
            max_text_bytes: model.max_text_bytes,
        },
    })
}

#[async_trait]
impl Gear for OnnxScanner {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        if self.state.get().is_some() {
            anyhow::bail!("{} gear already initialized", Self::MODULE_NAME);
        }

        let cfg: OnnxScannerConfig = ctx.config_or_default()?;
        cfg.validate()?;
        let thresholds = cfg.thresholds()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            backend = ?cfg.backend,
            "Loaded plugin configuration"
        );
        let spec = backend_spec(&cfg)?;

        // Meter obtained from the platform provider rather than a global
        // reference held here.
        let metrics_scope =
            opentelemetry::InstrumentationScope::builder(Self::MODULE_NAME.to_owned()).build();
        let metrics: Arc<dyn RuntimeMetricsPort> = Arc::new(RuntimeMetricsMeter::new(
            &opentelemetry::global::meter_with_scope(metrics_scope),
            "onnx_scanner",
        ));

        let models = load_models(
            &cfg.models,
            cfg.require_all_models,
            metrics,
            ctx.cancellation_token(),
        )
        .await
        .inspect_err(|e| {
            if e.leaked_init_thread() {
                // Surfaced because the process must now terminate for the
                // leak to stay bounded; the operator needs to know why a
                // clean exit may not be available.
                tracing::error!(
                    error = %e,
                    "onnx-scanner: model loading left an abandoned thread; the process must \
                     terminate"
                );
            }
        })
        .context("onnx-scanner: loading models")?;
        let runtime = Arc::new(LocalRuntime::new(models));

        let backend = build_backend(Arc::clone(&runtime) as Arc<dyn ModelRuntime>, spec)
            .await
            .context("onnx-scanner: building the PII backend")?;

        let service = Arc::new(Service::new(
            backend,
            cfg.category_prefix,
            InputLimits {
                max_words: cfg.max_words,
                max_bytes: cfg.max_payload_bytes,
            },
            thresholds,
        ));

        let (instance_id, instance_json) = PluginV1::<ScannerBackendSpecV1>::build_registration(
            "cf.builtin.onnx_pii.detector.v1",
            cfg.vendor,
            cfg.priority,
        )?;

        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        ctx.client_hub()
            .register_scoped::<dyn ScannerBackendClient>(
                ClientScope::gts_id(&instance_id),
                service,
            );

        self.state
            .set(GearState { runtime })
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        info!(instance_id = %instance_id, "onnx-scanner registered");
        Ok(())
    }
}

/// **Not an HTTP surface.** This impl exists solely for the `healthcheck` hook,
/// which is how the platform learns whether the models loaded. `register_rest`
/// returns the router it was handed, unchanged.
impl RestApiCapability for OnnxScanner {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        Ok(router)
    }

    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        // `None` before `init` has run, which is honest: there is no model
        // state to report yet, and the platform treats an absent check as an
        // opt-out rather than as health.
        let state = self.state.get()?;
        Some(Arc::new(ModelReadiness::new(Arc::clone(&state.runtime))))
    }
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;
    use toolkit::ClientHub;
    use toolkit::config::ConfigProvider;

    use super::*;

    struct OneGear(serde_json::Value);

    impl ConfigProvider for OneGear {
        fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
            (gear_name == "onnx-scanner").then_some(&self.0)
        }
    }

    fn ctx(config: &serde_json::Value) -> GearCtx {
        GearCtx::new(
            "onnx-scanner",
            uuid::Uuid::new_v4(),
            Arc::new(OneGear(serde_json::json!({ "config": config }))),
            Arc::new(ClientHub::default()),
            CancellationToken::new(),
        )
    }

    fn model(id: &str) -> serde_json::Value {
        serde_json::json!({
            "model_id": id,
            "model_dir": "/nonexistent",
            "skip_checksum": true,
            "expected_inputs": ["input_ids", "attention_mask"],
            "expected_outputs": ["logits"],
            "output_dtypes": { "logits": "f32" },
        })
    }

    /// Init must refuse a bad config before it loads any model, and leave the
    /// gear without state (so no health check is offered either).
    async fn assert_rejected(config: serde_json::Value, field: &str) {
        let gear = OnnxScanner::default();
        let err = gear
            .init(&ctx(&config))
            .await
            .expect_err("must be rejected");
        assert!(format!("{err:#}").contains(field), "{err:#}");
        assert!(gear.state.get().is_none());
    }

    #[tokio::test]
    async fn init_rejects_inverted_thresholds() {
        assert_rejected(
            serde_json::json!({
                "models": [model("minilm-l6-pii")],
                "redact_threshold": 0.2,
                "warn_threshold": 0.5,
            }),
            "redact_threshold/warn_threshold",
        )
        .await;
    }

    #[tokio::test]
    async fn init_rejects_a_backend_model_missing_from_models() {
        assert_rejected(
            serde_json::json!({ "models": [model("other")] }),
            "backend.model_id",
        )
        .await;
    }

    #[tokio::test]
    async fn init_rejects_a_zero_word_limit() {
        assert_rejected(
            serde_json::json!({ "models": [model("minilm-l6-pii")], "max_words": 0 }),
            "max_words",
        )
        .await;
    }

    #[test]
    fn gliner2_spec_takes_max_text_bytes_from_its_model() {
        let mut m = model("g");
        m["max_text_bytes"] = serde_json::json!(4096);
        let cfg: OnnxScannerConfig = serde_json::from_value(serde_json::json!({
            "models": [m],
            "backend": { "kind": "gliner2", "model_id": "g" },
        }))
        .expect("parses");
        let BackendSpec::Gliner2 { max_text_bytes, .. } = backend_spec(&cfg).expect("spec") else {
            panic!("expected a gliner2 spec");
        };
        assert_eq!(max_text_bytes, 4096);
    }
}
