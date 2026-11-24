#![expect(
    clippy::expect_used,
    reason = "test helpers fail loudly on a broken fixture rather than return errors"
)]

//! Payload content must never reach telemetry.
//!
//! This gear handles content that is attacker-controlled and confidential by
//! construction — that is the entire point of the program it belongs to. Logs
//! and spans are exactly where such content leaks, usually via a well-meant
//! debug field added during an unrelated change.
//!
//! So this is a control, not a check: a distinctive sentinel string is pushed
//! through every path the gear has, and every captured span and event is
//! searched for it. Adding `text = %text` to any instrumented function makes
//! this suite fail. **Treat its removal or weakening as a defect.**

mod common;

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use common::{MODEL_ID, fixture_config, fixture_with, no_metrics};
use onnx_scanner::runtime::{LocalRuntime, load_models};
use onnx_scanner::runtime::{ModelRuntime, NamedTensor, TensorData};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::layer::SubscriberExt;

/// A string that cannot plausibly occur by accident, so any appearance in
/// telemetry is proof it came from the payload.
const SENTINEL: &str = "zzQUUXSENTINEL7431zz";

/// Tensor values that cannot plausibly occur in telemetry by accident either:
/// an id and a weight nobody would log for any other reason.
const SENTINEL_ID: i64 = 7_431_987_654_321;
const SENTINEL_ID_TEXT: &str = "7431987654321";
const SENTINEL_WEIGHT: f32 = 0.743_198_7;
const SENTINEL_WEIGHT_TEXT: &str = "0.7431987";

/// Captures every field of every event *and* span emitted anywhere in the
/// process, flattened to text. Deliberately indiscriminate: a leak is a leak
/// wherever it appears.
///
/// Installed once as the **global** subscriber. A thread-local default would
/// miss everything emitted on `spawn_blocking` threads and other runtime
/// workers — exactly where inference and tokenization run.
#[derive(Clone, Default)]
struct CapturingLayer {
    captured: Arc<Mutex<Vec<String>>>,
}

impl CapturingLayer {
    fn push(&self, line: String) {
        self.captured
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line);
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CapturingLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        event.record(&mut Flatten(self.clone()));
        self.push(event.metadata().name().to_owned());
    }

    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _: &tracing::Id,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) {
        attrs.record(&mut Flatten(self.clone()));
        self.push(attrs.metadata().name().to_owned());
    }

    fn on_record(
        &self,
        _: &tracing::Id,
        values: &tracing::span::Record<'_>,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) {
        values.record(&mut Flatten(self.clone()));
    }
}

struct Flatten(CapturingLayer);

impl tracing::field::Visit for Flatten {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{}={value:?}", field.name()));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.push(format!("{}={value}", field.name()));
    }
}

/// The process-wide capture buffer, installing the global subscriber on first
/// use.
fn capture_buffer() -> &'static Arc<Mutex<Vec<String>>> {
    static BUFFER: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();
    BUFFER.get_or_init(|| {
        let layer = CapturingLayer::default();
        let captured = Arc::clone(&layer.captured);
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer))
            .expect("no other global subscriber in this test binary");
        captured
    })
}

/// Tests share one global buffer, so they run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Runs `body` with telemetry captured from every thread, and returns
/// everything recorded while it ran.
async fn capture<F, Fut>(body: F) -> Vec<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let _serial = SERIAL.lock().await;
    let buffer = capture_buffer();
    buffer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    body().await;
    // Let any event from a blocking thread that outlived its await land.
    tokio::task::yield_now().await;
    buffer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn assert_no_leak(captured: &[String], context: &str) {
    let leaked: Vec<&String> = captured
        .iter()
        .filter(|c| {
            c.contains(SENTINEL) || c.contains(SENTINEL_ID_TEXT) || c.contains(SENTINEL_WEIGHT_TEXT)
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "payload content reached telemetry on the {context} path: {leaked:?}\n\
         This is a security control, not a style rule — see this file's module docs."
    );
    assert!(
        !captured.is_empty(),
        "captured nothing on the {context} path; the test would pass vacuously"
    );
}

async fn client() -> LocalRuntime {
    let entries = load_models(
        &[fixture_config()],
        true,
        no_metrics(),
        &CancellationToken::new(),
    )
    .await
    .expect("fixture loads");
    LocalRuntime::new(entries)
}

/// Tensor *names* are safe and useful in telemetry; tensor *values* are not.
/// The values are distinctive, so their appearance anywhere is a leak.
fn sentinel_tensors() -> Vec<NamedTensor> {
    vec![
        NamedTensor::new("ids", vec![1, 2], TensorData::I64(vec![SENTINEL_ID; 2])),
        NamedTensor::new(
            "weights",
            vec![1, 2],
            TensorData::F32(vec![SENTINEL_WEIGHT; 2]),
        ),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tokenizing_does_not_leak_the_text() {
    let captured = capture(|| async {
        let client = client().await;
        let text = format!("prefix {SENTINEL} suffix");
        drop(client.encode(MODEL_ID, &text).await);
        drop(client.encode_fragment(MODEL_ID, SENTINEL).await);
    })
    .await;

    assert_no_leak(&captured, "tokenize");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_text_does_not_leak_on_rejection() {
    let captured = capture(|| async {
        let entries = load_models(
            &[fixture_with(|c| c.max_text_bytes = 8)],
            true,
            no_metrics(),
            &CancellationToken::new(),
        )
        .await
        .expect("loads");
        let client = LocalRuntime::new(entries);

        let text = format!("{SENTINEL}{SENTINEL}");
        let err = client.encode(MODEL_ID, &text).await.unwrap_err();
        // The error itself must not carry the text either — it is logged by
        // whoever receives it.
        assert!(
            !err.to_string().contains(SENTINEL),
            "the error leaked the text"
        );
    })
    .await;

    assert_no_leak(&captured, "oversized-text rejection");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successful_inference_does_not_leak_tensor_values() {
    let captured = capture(|| async {
        let client = client().await;
        drop(client.run(MODEL_ID, sentinel_tensors()).await);
    })
    .await;

    assert_no_leak(&captured, "inference");
}

/// A tensor *name* is caller-supplied too, and it reaches the contract error.
/// Names are permitted in telemetry by the feature's field list — this test
/// pins that the *values* still do not appear, and doubles as a check that an
/// error path logs nothing extra.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_input_does_not_leak_tensor_values() {
    let captured = capture(|| async {
        let client = client().await;
        let bad = vec![NamedTensor::new(
            "ids",
            vec![9, 9],
            TensorData::I64(vec![SENTINEL_ID, SENTINEL_ID]),
        )];
        drop(client.run(MODEL_ID, bad).await);
    })
    .await;

    assert_no_leak(&captured, "invalid input");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unavailable_model_does_not_leak() {
    let captured = capture(|| async {
        let entries = load_models(
            &[fixture_with(|c| c.model_file = "absent.onnx".to_owned())],
            false,
            no_metrics(),
            &CancellationToken::new(),
        )
        .await
        .expect("startup stays non-fatal");
        let client = LocalRuntime::new(entries);

        drop(client.encode(MODEL_ID, SENTINEL).await);
        drop(client.run(MODEL_ID, sentinel_tensors()).await);
    })
    .await;

    assert_no_leak(&captured, "unavailable model");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_model_does_not_leak() {
    let captured = capture(|| async {
        let client = client().await;
        drop(client.encode("no-such-model", SENTINEL).await);
    })
    .await;

    assert_no_leak(&captured, "unknown model");
}

/// The test has teeth only if it can actually see a leak — including one
/// emitted on a blocking-pool thread, where inference and tokenization run.
/// This proves the capture layer works by leaking on purpose into the same
/// machinery, on both kinds of thread.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_capture_layer_would_catch_a_leak() {
    let captured = capture(|| async {
        tracing::info!(text = %SENTINEL, "a deliberately careless log line");
    })
    .await;
    assert!(
        captured.iter().any(|c| c.contains(SENTINEL)),
        "the capture layer missed an obvious leak, so every other test here is vacuous"
    );

    let captured = capture(|| async {
        tokio::task::spawn_blocking(|| {
            tracing::info!(
                value = SENTINEL_ID,
                "a careless log line on a blocking thread"
            );
        })
        .await
        .expect("joins");
    })
    .await;
    assert!(
        captured.iter().any(|c| c.contains(SENTINEL_ID_TEXT)),
        "the capture layer missed a leak on a spawn_blocking thread: {captured:?}"
    );
}
