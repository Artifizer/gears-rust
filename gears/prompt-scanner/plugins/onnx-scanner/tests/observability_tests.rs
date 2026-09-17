#![expect(
    clippy::expect_used,
    reason = "test helpers fail loudly on a broken fixture rather than return errors"
)]

//! What the gear reports about itself, and what it must never report.
//!
//! Two things are proven here. First, that queue wait and execution time are
//! genuinely *separate* measurements rather than one number recorded twice —
//! an operator uses them to tell "too few replicas" from "the model is slow",
//! and a benchmark uses them to attribute cost. Second, that payload content
//! cannot reach telemetry.
//!
//! Runs against the committed fixture, so no model download is needed.

mod common;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use common::{MODEL_ID, RecordingMetrics, fixture_config, fixture_with, no_metrics};
use onnx_scanner::runtime::config::OverloadPolicy;
use onnx_scanner::runtime::metrics_port::InferenceOutcome;
use onnx_scanner::runtime::{LocalRuntime, load_models};
use onnx_scanner::runtime::{ModelRuntime, NamedTensor, TensorData};
use tokio_util::sync::CancellationToken;

fn inputs(n: usize) -> Vec<NamedTensor> {
    vec![
        NamedTensor::new("ids", vec![1, n], TensorData::I64(vec![1; n])),
        NamedTensor::new("weights", vec![1, n], TensorData::F32(vec![0.5; n])),
    ]
}

async fn client_with(
    cfg: onnx_scanner::runtime::config::ModelConfig,
    metrics: Arc<RecordingMetrics>,
) -> LocalRuntime {
    let entries = load_models(&[cfg], true, metrics, &CancellationToken::new())
        .await
        .expect("fixture loads");
    LocalRuntime::new(entries)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_successful_inference_records_duration_and_outcome() {
    let metrics = Arc::new(RecordingMetrics::default());
    let client = client_with(fixture_config(), Arc::clone(&metrics)).await;

    client.run(MODEL_ID, inputs(8)).await.expect("runs");

    let recorded = metrics.inferences();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].model_id, MODEL_ID);
    assert_eq!(recorded[0].outcome, InferenceOutcome::Ok);
    assert!(
        recorded[0].timing.execution > Duration::ZERO,
        "a call that ran reports its execution time"
    );
}

/// Queue wait is a real measurement of waiting, not a copy of execution and
/// not a constant: with the only replica held for a known time, the recorded
/// wait covers that time and the execution does not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_wait_measures_time_spent_waiting_for_a_replica() {
    const HELD: Duration = Duration::from_millis(150);
    let metrics = Arc::new(RecordingMetrics::default());
    let client = Arc::new(client_with(fixture_config(), Arc::clone(&metrics)).await);

    let hold = client.hold_replica(MODEL_ID).await.expect("holds");
    let call = {
        let client = Arc::clone(&client);
        tokio::spawn(async move { client.run(MODEL_ID, inputs(8)).await })
    };
    tokio::time::sleep(HELD).await;
    drop(hold);
    call.await.expect("joins").expect("runs");

    let recorded = metrics.inferences();
    assert_eq!(recorded.len(), 1);
    let timing = recorded[0].timing;
    assert!(
        timing.queue_wait >= HELD.saturating_sub(Duration::from_millis(10)),
        "the wait for the held replica is queue wait: {timing:?}"
    );
    assert!(
        timing.execution < HELD,
        "and is not counted as execution: {timing:?}"
    );
}

/// The other direction: an uncontended call waits for nothing, so its queue
/// wait is small next to the wait above.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queue_wait_is_near_zero_when_idle() {
    let metrics = Arc::new(RecordingMetrics::default());
    let client = client_with(fixture_config(), Arc::clone(&metrics)).await;

    client.run(MODEL_ID, inputs(8)).await.expect("runs");

    let recorded = metrics.inferences();
    assert_eq!(recorded.len(), 1);
    assert!(
        recorded[0].timing.queue_wait < Duration::from_millis(50),
        "an uncontended call should not queue: {:?}",
        recorded[0].timing
    );
}

/// A gauge that drifts is worse than no gauge. Every increment must be paired
/// — including for a call rejected before execution.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn in_flight_returns_to_zero_including_on_rejection() {
    let metrics = Arc::new(RecordingMetrics::default());
    let client = client_with(fixture_config(), Arc::clone(&metrics)).await;

    client.run(MODEL_ID, inputs(8)).await.expect("runs");
    let bad = vec![NamedTensor::new(
        "ids",
        vec![9, 9],
        TensorData::I64(vec![1]),
    )];
    assert!(client.run(MODEL_ID, bad).await.is_err());

    assert_eq!(
        metrics.in_flight_deltas(),
        vec![1, -1, 1, -1],
        "one increment and one decrement per call, in order"
    );
}

/// A call shed at the ceiling is recorded as `overloaded`, with no execution.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shed_call_is_recorded_as_overloaded() {
    let metrics = Arc::new(RecordingMetrics::default());
    let client = Arc::new(
        client_with(
            fixture_with(|c| {
                c.max_in_flight = NonZeroUsize::new(1);
                c.on_overload = OverloadPolicy::Reject;
            }),
            Arc::clone(&metrics),
        )
        .await,
    );

    let hold = client.hold_replica(MODEL_ID).await.expect("holds");
    let waiting = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            loop {
                match client.run(MODEL_ID, inputs(2)).await {
                    Err(onnx_scanner::runtime::RuntimeError::Overloaded { .. }) => {
                        tokio::task::yield_now().await;
                    }
                    other => break other,
                }
            }
        })
    };
    // Until the spawned call holds the only slot, a probe may take it; a
    // bounded probe gives it back.
    loop {
        let probe =
            tokio::time::timeout(Duration::from_millis(50), client.run(MODEL_ID, inputs(2))).await;
        if matches!(
            probe,
            Ok(Err(onnx_scanner::runtime::RuntimeError::Overloaded { .. }))
        ) {
            break;
        }
    }
    drop(hold);
    drop(waiting.await.expect("joins"));

    let overloaded: Vec<_> = metrics
        .inferences()
        .into_iter()
        .filter(|r| r.outcome == InferenceOutcome::Overloaded)
        .collect();
    assert!(!overloaded.is_empty());
    assert!(
        overloaded
            .iter()
            .all(|r| r.timing.execution == Duration::ZERO),
        "a shed call never executed: {overloaded:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_input_is_recorded_with_its_own_outcome() {
    let metrics = Arc::new(RecordingMetrics::default());
    let client = client_with(fixture_config(), Arc::clone(&metrics)).await;

    let bad = vec![NamedTensor::new(
        "ids",
        vec![2, 3],
        TensorData::I64(vec![1; 5]),
    )];
    assert!(client.run(MODEL_ID, bad).await.is_err());

    let recorded = metrics.inferences();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].outcome,
        InferenceOutcome::InvalidInput,
        "a caller's malformed request is distinguishable from the model failing"
    );
    assert_eq!(
        recorded[0].timing,
        onnx_scanner::runtime::metrics_port::InferenceTiming::default(),
        "a call rejected before execution reports neither wait nor execution"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_load_is_recorded_with_its_footprint() {
    let metrics = Arc::new(RecordingMetrics::default());
    let _client = client_with(fixture_config(), Arc::clone(&metrics)).await;

    let loads = metrics.loads();
    let file_size = std::fs::metadata(common::fixtures_dir().join("tiny.onnx"))
        .expect("fixture exists")
        .len();
    assert_eq!(
        loads,
        vec![(MODEL_ID.to_owned(), "ok".to_owned(), file_size)],
        "one replica: the estimate is the file size"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tokenization_is_measured_separately_from_inference() {
    let metrics = Arc::new(RecordingMetrics::default());
    let client = client_with(fixture_config(), Arc::clone(&metrics)).await;

    client
        .encode(MODEL_ID, "hello world")
        .await
        .expect("encodes");

    assert_eq!(
        metrics.tokenizations(),
        vec![(MODEL_ID.to_owned(), InferenceOutcome::Ok)]
    );
    assert!(
        metrics.inferences().is_empty(),
        "tokenizing is not inference and must not be counted as it"
    );
}

/// Metrics must never be a prerequisite for running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_gear_works_without_a_metrics_backend() {
    let entries = load_models(
        &[fixture_config()],
        true,
        no_metrics(),
        &CancellationToken::new(),
    )
    .await
    .expect("loads");
    let client = LocalRuntime::new(entries);

    assert!(client.run(MODEL_ID, inputs(4)).await.is_ok());
}
