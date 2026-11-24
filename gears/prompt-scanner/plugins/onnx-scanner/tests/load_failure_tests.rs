#![expect(
    clippy::expect_used,
    reason = "test helpers fail loudly on a broken fixture rather than return errors"
)]

//! Load failures that are classified before any ONNX Runtime session is
//! opened, against the committed fixture.
//!
//! Every config here fails at the config, model-file or tokenizer step, so
//! these run in the default `cargo test` with no `ORT_DYLIB_PATH`. A case that
//! reached a session open would hang without a runtime — which is why each
//! one breaks something checked earlier.

mod common;

use std::sync::Arc;

use common::{MODEL_ID, RecordingMetrics, digest, fixture_config_without_runtime, no_metrics};
use onnx_scanner::runtime::config::{ConfigError, ModelConfig};
use onnx_scanner::runtime::health::ModelReadiness;
use onnx_scanner::runtime::metrics_port::RuntimeMetricsPort;
use onnx_scanner::runtime::session::LoadFailureSource;
use onnx_scanner::runtime::{
    InitError, LocalRuntime, ModelFailureReason, ModelRuntime, RuntimeError, load_models,
};
use tokio_util::sync::CancellationToken;
use toolkit::{Healthcheck, HealthcheckStatus};

fn with(mutate: impl FnOnce(&mut ModelConfig)) -> ModelConfig {
    let mut cfg = fixture_config_without_runtime();
    mutate(&mut cfg);
    cfg
}

fn missing_model() -> ModelConfig {
    with(|c| "absent.onnx".clone_into(&mut c.model_file))
}

/// The tiny model with a tokenizer that is not a tokenizer: the model file is
/// hashed and verified, then the tokenizer step fails before any open.
fn bad_tokenizer() -> ModelConfig {
    with(|c| {
        "README.md".clone_into(&mut c.tokenizer_file);
        c.tokenizer_sha256 = None;
    })
}

async fn load(models: &[ModelConfig], require_all: bool) -> Result<LocalRuntime, InitError> {
    load_models(models, require_all, no_metrics(), &CancellationToken::new())
        .await
        .map(LocalRuntime::new)
}

async fn reason_of(cfg: ModelConfig) -> ModelFailureReason {
    let runtime = load(&[cfg], false).await.expect("startup stays non-fatal");
    let models = runtime.list_models().await.expect("lists");
    assert_eq!(models.len(), 1, "the failed model is still listed");
    models[0]
        .failure_reason()
        .expect("a model that failed to load is unavailable")
}

#[tokio::test]
async fn each_pre_session_failure_has_its_own_reason() {
    let cases = [
        (missing_model(), ModelFailureReason::FileUnreadable),
        (
            with(|c| {
                c.checksum =
                    onnx_scanner::runtime::config::Checksum::Sha256(digest(&"0".repeat(64)));
            }),
            ModelFailureReason::ChecksumMismatch,
        ),
        (bad_tokenizer(), ModelFailureReason::TokenizerInvalid),
        (
            with(|c| c.tokenizer_sha256 = Some(digest(&"1".repeat(64)))),
            ModelFailureReason::TokenizerInvalid,
        ),
        (with(|c| c.replicas = 0), ModelFailureReason::InvalidConfig),
        (
            with(|c| "../fixtures/tiny.onnx".clone_into(&mut c.model_file)),
            ModelFailureReason::InvalidConfig,
        ),
        (
            with(|c| {
                c.output_dtypes.remove("scores");
            }),
            ModelFailureReason::InvalidConfig,
        ),
    ];
    for (cfg, expected) in cases {
        assert_eq!(reason_of(cfg).await, expected);
    }
}

/// The escape hatch for deployments that would rather not start than start
/// degraded — exercised on the fixture, not only against a real model.
#[tokio::test]
async fn require_all_models_fails_startup_naming_the_model_and_cause() {
    let Err(err) = load(&[missing_model()], true).await else {
        panic!("require_all_models must fail startup");
    };
    let InitError::Model(failure) = &err else {
        panic!("expected a per-model failure, got {err}");
    };
    assert_eq!(failure.model_id, MODEL_ID);
    assert_eq!(failure.source.reason, ModelFailureReason::FileUnreadable);
    assert!(
        matches!(*failure.source.source, LoadFailureSource::Io { .. }),
        "the io::Error is kept, typed: {err}"
    );
    assert!(!err.leaked_init_thread());
}

#[tokio::test]
async fn require_all_models_also_fails_on_a_bad_tokenizer() {
    let Err(InitError::Model(failure)) = load(&[bad_tokenizer()], true).await else {
        panic!("a bad tokenizer must fail startup under require_all_models");
    };
    assert_eq!(failure.source.reason, ModelFailureReason::TokenizerInvalid);
}

#[tokio::test]
async fn duplicate_model_ids_fail_startup_even_when_permissive() {
    let Err(err) = load(&[missing_model(), missing_model()], false).await else {
        panic!("a repeated id must fail startup");
    };
    assert!(
        matches!(&err, InitError::InvalidConfig(ConfigError::DuplicateModelId { model_id })
            if model_id == MODEL_ID),
        "{err}"
    );
}

#[tokio::test]
async fn a_malformed_model_id_fails_startup() {
    let Err(err) = load(
        &[with(|c| "bad id/../x".clone_into(&mut c.model_id))],
        false,
    )
    .await
    else {
        panic!("a malformed id must fail startup");
    };
    assert!(
        matches!(
            err,
            InitError::InvalidConfig(ConfigError::InvalidModelId { .. })
        ),
        "{err}"
    );
}

#[tokio::test]
async fn shutdown_before_loading_abandons_nothing() {
    let token = CancellationToken::new();
    token.cancel();
    let err = load_models(&[missing_model()], false, no_metrics(), &token)
        .await
        .err();
    // Either the cancellation or the (instant) failed load may win the race;
    // whichever does, no session open was in flight to abandon.
    if let Some(err) = err {
        assert!(
            matches!(
                err,
                InitError::Cancelled {
                    open_in_flight: false
                }
            ),
            "{err}"
        );
        assert!(!err.leaked_init_thread());
    }
}

#[tokio::test]
async fn unknown_and_unavailable_are_distinguishable() {
    let runtime = load(&[missing_model()], false).await.expect("non-fatal");

    let unavailable = runtime.run(MODEL_ID, Vec::new()).await.unwrap_err();
    assert!(
        matches!(&unavailable, RuntimeError::ModelUnavailable { reason, .. }
            if *reason == ModelFailureReason::FileUnreadable),
        "a configured-but-broken model is an outage: {unavailable}"
    );

    let unknown = runtime.encode("typo", "hello").await.unwrap_err();
    assert!(
        matches!(unknown, RuntimeError::UnknownModel { .. }),
        "an unconfigured id is a caller mistake: {unknown}"
    );
}

#[tokio::test]
async fn a_failed_load_is_recorded_with_its_reason_label() {
    let metrics = Arc::new(RecordingMetrics::default());
    load_models(
        &[
            missing_model(),
            with(|c| {
                "bad-tokenizer".clone_into(&mut c.model_id);
                "README.md".clone_into(&mut c.tokenizer_file);
                c.tokenizer_sha256 = None;
            }),
        ],
        false,
        Arc::clone(&metrics) as Arc<dyn RuntimeMetricsPort>,
        &CancellationToken::new(),
    )
    .await
    .expect("startup stays non-fatal");

    let loads = metrics.loads();
    assert_eq!(
        loads,
        vec![
            (MODEL_ID.to_owned(), "file_unreadable".to_owned(), 0),
            (
                "bad-tokenizer".to_owned(),
                "tokenizer_invalid".to_owned(),
                0
            ),
        ]
    );
}

#[tokio::test]
async fn readiness_names_each_failed_model_with_its_code() {
    let runtime = load(
        &[
            missing_model(),
            with(|c| {
                "tampered".clone_into(&mut c.model_id);
                c.checksum =
                    onnx_scanner::runtime::config::Checksum::Sha256(digest(&"f".repeat(64)));
            }),
        ],
        false,
    )
    .await
    .expect("non-fatal");

    let result = ModelReadiness::new(Arc::new(runtime)).check().await;
    assert_eq!(result.status, HealthcheckStatus::Unhealthy);
    assert_eq!(result.code.as_deref(), Some("no_models_ready"));
    assert_eq!(
        result.message.as_deref(),
        Some(
            "no models ready (2 configured): tampered (checksum_mismatch), tiny (file_unreadable)"
        )
    );
}
