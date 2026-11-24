#![expect(
    clippy::expect_used,
    reason = "test helpers fail loudly on a broken fixture rather than return errors"
)]

//! Hosting several distinct models at once.
//!
//! The gear's whole purpose is being a *generic* runner, and that claim has
//! never been executed: until now every test loaded one model. This suite
//! hosts two graphs that deliberately disagree — different tensor names,
//! different arity of dtypes, different digests — because two copies of the
//! same model would pass a contract-isolation test that no isolation would
//! also pass.
//!
//! Its external acceptance test lives outside this repo: a consumer must be
//! able to add a second model by **configuration only**, with no source change
//! here. Everything below is written against the public config and client
//! surfaces for that reason.

mod common;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use common::{
    MODEL_ID, OTHER_MODEL_ID, digest, fixture_config, no_metrics, other_fixture_config,
    other_fixture_with,
};
use onnx_scanner::runtime::config::{Checksum, OverloadPolicy};
use onnx_scanner::runtime::{LocalRuntime, load_models};
use onnx_scanner::runtime::{ModelIntegrity, ModelRuntime, NamedTensor, RuntimeError, TensorData};
use tokio_util::sync::CancellationToken;

/// Both fixtures, loaded exactly as the gear loads them.
async fn two_model_client() -> LocalRuntime {
    client_over(vec![fixture_config(), other_fixture_config()]).await
}

async fn client_over(configs: Vec<onnx_scanner::runtime::config::ModelConfig>) -> LocalRuntime {
    let models = load_models(&configs, false, no_metrics(), &CancellationToken::new())
        .await
        .expect("permissive load never fails");
    LocalRuntime::new(models)
}

fn tiny_inputs(n: usize) -> Vec<NamedTensor> {
    let ids: Vec<i64> = sequential_ids(n);
    let weights: Vec<f32> = (0..n).map(|i| f32::from(u8_of(i)) * 0.5).collect();
    vec![
        NamedTensor::new("ids", vec![1, n], TensorData::I64(ids)),
        NamedTensor::new("weights", vec![1, n], TensorData::F32(weights)),
    ]
}

/// `0..n` as `i64`, without an `as` cast — the request path forbids them, and
/// a test that builds its inputs should not model what the gear refuses to do.
fn sequential_ids(n: usize) -> Vec<i64> {
    (0..n)
        .map(|i| i64::try_from(i).expect("test sizes are small"))
        .collect()
}

fn u8_of(i: usize) -> u8 {
    u8::try_from(i % 251).unwrap_or(0)
}

fn other_inputs(n: usize) -> Vec<NamedTensor> {
    let tokens: Vec<u8> = (0..n).map(u8_of).collect();
    let marks: Vec<bool> = (0..n).map(|i| i % 3 == 0).collect();
    vec![
        NamedTensor::new("tokens", vec![1, n], TensorData::U8(tokens)),
        NamedTensor::new("marks", vec![1, n], TensorData::Bool(marks)),
    ]
}

fn named<'a>(out: &'a [NamedTensor], name: &str) -> &'a NamedTensor {
    out.iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("{name} missing from outputs"))
}

// --- Both models hosted, each with its own everything -----------------------

#[tokio::test]
async fn both_models_load_with_their_own_contracts_and_digests() {
    let client = two_model_client().await;
    let models = client.list_models().await.expect("lists");

    assert_eq!(models.len(), 2, "both configured models are reported");

    let tiny = models
        .iter()
        .find(|m| m.model_id == MODEL_ID)
        .expect("tiny listed");
    let other = models
        .iter()
        .find(|m| m.model_id == OTHER_MODEL_ID)
        .expect("other listed");

    assert!(tiny.is_ready() && other.is_ready(), "both loaded");

    let tiny_loaded = tiny.loaded().expect("ready implies loaded detail");
    let other_loaded = other.loaded().expect("ready implies loaded detail");

    assert_eq!(tiny_loaded.input_names, vec!["ids", "weights"]);
    assert_eq!(tiny_loaded.output_names, vec!["ids_out", "scores"]);
    assert_eq!(other_loaded.input_names, vec!["tokens", "marks"]);
    assert_eq!(other_loaded.output_names, vec!["tokens_out", "marks_out"]);

    assert_eq!(
        tiny_loaded.integrity,
        ModelIntegrity::Verified {
            sha256: common::FIXTURE_SHA256.to_owned()
        }
    );
    assert_eq!(
        other_loaded.integrity,
        ModelIntegrity::Verified {
            sha256: common::OTHER_FIXTURE_SHA256.to_owned()
        },
        "distinct artifacts, verified independently"
    );
}

#[tokio::test]
async fn each_model_runs_against_its_own_graph() {
    let client = two_model_client().await;

    let tiny_out = client
        .run(MODEL_ID, tiny_inputs(4))
        .await
        .expect("tiny runs");
    assert_eq!(
        named(&tiny_out, "ids_out").data,
        TensorData::I64(vec![0, 1, 2, 3])
    );
    assert_eq!(
        named(&tiny_out, "scores").data,
        TensorData::F32(vec![0.0, 0.5, 1.0, 1.5])
    );

    let other_out = client
        .run(OTHER_MODEL_ID, other_inputs(4))
        .await
        .expect("other runs");
    assert_eq!(
        named(&other_out, "tokens_out").data,
        TensorData::U8(vec![0, 1, 2, 3])
    );
    assert_eq!(
        named(&other_out, "marks_out").data,
        TensorData::Bool(vec![true, false, false, true])
    );
}

#[tokio::test]
async fn one_models_tensors_are_rejected_by_the_other() {
    // The isolation that matters: dispatching by id must not mean "any
    // configured graph will do". Feeding tiny's inputs to `other` is a
    // contract violation, not a shape error discovered inside ONNX Runtime.
    let client = two_model_client().await;
    let err = client
        .run(OTHER_MODEL_ID, tiny_inputs(2))
        .await
        .expect_err("wrong model's tensors must be rejected");

    assert!(
        matches!(&err, RuntimeError::ContractMismatch { model_id, .. } if model_id == OTHER_MODEL_ID),
        "a contract violation naming the model that rejected them, not a shape error from \
         inside ONNX Runtime: {err}"
    );
    assert!(
        !err.is_retryable(),
        "a contract violation is terminal: {err}"
    );
}

#[tokio::test]
async fn an_unknown_id_is_distinct_from_an_unavailable_model() {
    // Three outcomes that must stay apart: served, configured-but-broken, and
    // never configured. Collapsing the last two is what makes a typo look like
    // an outage.
    let broken = other_fixture_with(|c| {
        c.model_id = "broken".to_owned();
        c.checksum = Checksum::Sha256(digest(&"f".repeat(64)));
    });
    let client = client_over(vec![fixture_config(), broken]).await;

    let unknown = client
        .run("never-configured", tiny_inputs(2))
        .await
        .expect_err("unknown id fails");
    assert!(
        matches!(unknown, RuntimeError::UnknownModel { .. }),
        "got {unknown:?}"
    );

    let unavailable = client
        .run("broken", other_inputs(2))
        .await
        .expect_err("unavailable model fails");
    assert!(
        matches!(unavailable, RuntimeError::ModelUnavailable { .. }),
        "got {unavailable:?}"
    );

    assert!(
        client.run(MODEL_ID, tiny_inputs(2)).await.is_ok(),
        "the good one still serves"
    );
}

#[tokio::test]
async fn a_contract_mismatch_on_the_second_model_names_that_model() {
    // A generic startup error would send an operator reading the wrong config
    // block. The first model is fine; the second declares an output its graph
    // does not have.
    //
    // The declared output needs a dtype as well as a name, or the config check
    // rejects it as `invalid_config` before a session is ever built — which is
    // correct behaviour, and a different failure from the one under test here.
    let wrong = other_fixture_with(|c| {
        c.expected_outputs = vec!["tokens_out".to_owned(), "not_in_the_graph".to_owned()];
        c.output_dtypes.insert(
            "not_in_the_graph".to_owned(),
            onnx_scanner::runtime::config::OutputDtype::U8,
        );
    });
    let client = client_over(vec![fixture_config(), wrong]).await;
    let models = client.list_models().await.expect("lists");

    let tiny = models
        .iter()
        .find(|m| m.model_id == MODEL_ID)
        .expect("listed");
    let other = models
        .iter()
        .find(|m| m.model_id == OTHER_MODEL_ID)
        .expect("listed even though it failed");

    assert!(
        tiny.is_ready(),
        "one bad model does not take the other down"
    );
    assert!(!other.is_ready());

    let err = client
        .run(OTHER_MODEL_ID, other_inputs(2))
        .await
        .expect_err("unavailable");
    let rendered = err.to_string();
    assert!(
        rendered.contains(OTHER_MODEL_ID),
        "names which model: {rendered}"
    );
    assert!(
        rendered.contains("contract_mismatch"),
        "and why, in the stable vocabulary: {rendered}"
    );
}

/// Each model resolves its tokenizer through its own config. The second model
/// gets a tokenizer whose vocabulary swaps two ids, so the same text must
/// encode differently — a shared tokenizer would make the two agree.
#[tokio::test]
async fn tokenizers_are_per_model() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::copy(
        common::fixtures_dir().join("other.onnx"),
        dir.path().join("other.onnx"),
    )
    .expect("copies the model");
    let swapped = std::fs::read_to_string(common::tokenizer_path())
        .expect("reads the fixture tokenizer")
        .replace("\"hello\": 3", "\"hello\": 99")
        .replace("\"world\": 4", "\"world\": 3")
        .replace("\"hello\": 99", "\"hello\": 4");
    assert!(
        swapped.contains("\"hello\": 4") && swapped.contains("\"world\": 3"),
        "the fixture tokenizer's vocabulary layout changed; update the swap"
    );
    std::fs::write(dir.path().join("tokenizer.json"), swapped).expect("writes");

    let other = other_fixture_with(|c| {
        c.model_dir = dir.path().to_owned();
        c.tokenizer_sha256 = None;
    });
    let client = client_over(vec![fixture_config(), other]).await;

    let a = client
        .encode(MODEL_ID, "hello world")
        .await
        .expect("tiny encodes");
    let b = client
        .encode(OTHER_MODEL_ID, "hello world")
        .await
        .expect("other encodes");

    assert_eq!(a.ids, vec![1, 3, 4, 2], "fixture vocabulary");
    assert_eq!(b.ids, vec![1, 4, 3, 2], "the second model's own vocabulary");
}

// --- Concurrency across and within models -----------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_across_both_models_all_return_their_own_outputs() {
    let client = Arc::new(
        client_over(vec![
            common::fixture_with(|c| c.replicas = 2),
            other_fixture_with(|c| c.replicas = 2),
        ])
        .await,
    );

    let mut tasks = Vec::new();
    for i in 0..24_usize {
        let client = Arc::clone(&client);
        // Interleaved, and each call carries a payload unique to it, so a
        // crossed response is detectable rather than merely improbable.
        let n = 1 + i % 5;
        tasks.push(tokio::spawn(async move {
            if i % 2 == 0 {
                let out = client.run(MODEL_ID, tiny_inputs(n)).await?;
                let expected: Vec<i64> = sequential_ids(n);
                assert_eq!(named(&out, "ids_out").data, TensorData::I64(expected));
            } else {
                let out = client.run(OTHER_MODEL_ID, other_inputs(n)).await?;
                let expected: Vec<bool> = (0..n).map(|k| k % 3 == 0).collect();
                assert_eq!(named(&out, "marks_out").data, TensorData::Bool(expected));
            }
            Ok::<_, RuntimeError>(())
        }));
    }

    for task in tasks {
        task.await
            .expect("no task panicked")
            .expect("every call succeeds");
    }
}

/// Per-model pools and ceilings: a model that is completely saturated — its
/// only replica held and its single in-flight slot shed under `Reject` — must
/// not cost the other model anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_saturated_model_does_not_block_the_other() {
    let client = client_over(vec![
        common::fixture_with(|c| {
            c.replicas = 1;
            c.max_in_flight = NonZeroUsize::new(1);
            c.on_overload = OverloadPolicy::Reject;
        }),
        other_fixture_with(|c| {
            c.replicas = 1;
            c.max_in_flight = NonZeroUsize::new(1);
            c.on_overload = OverloadPolicy::Reject;
        }),
    ])
    .await;

    // Tiny's replica is held, so a call takes its one in-flight slot and
    // waits; a second call is shed.
    let hold = client.hold_replica(MODEL_ID).await.expect("holds tiny");
    let client = Arc::new(client);
    let waiting = {
        let client = Arc::clone(&client);
        tokio::spawn(async move {
            loop {
                match client.run(MODEL_ID, tiny_inputs(2)).await {
                    // The probe below briefly held the slot; try again.
                    Err(RuntimeError::Overloaded { .. }) => tokio::task::yield_now().await,
                    other => break other,
                }
            }
        })
    };
    let shed = loop {
        // Bounded, so that if this call wins the slot instead of the spawned
        // one it gives the slot back rather than waiting out the deadline.
        let attempt = tokio::time::timeout(
            Duration::from_millis(100),
            client.run(MODEL_ID, tiny_inputs(2)),
        )
        .await;
        if let Ok(Err(e @ RuntimeError::Overloaded { .. })) = attempt {
            break e;
        }
        tokio::task::yield_now().await;
    };
    assert!(matches!(
        shed,
        RuntimeError::Overloaded {
            max_in_flight: 1,
            ..
        }
    ));

    // With tiny fully saturated, the other model still answers at once.
    let other = client
        .run(OTHER_MODEL_ID, other_inputs(4))
        .await
        .expect("the other model answers while tiny is saturated");
    assert_eq!(named(&other, "tokens_out").shape, vec![1, 4]);

    drop(hold);
    assert!(
        waiting.await.expect("no task panicked").is_ok(),
        "the queued tiny call completes once its replica is released"
    );
}
