//! Load, verification, inference and dtype handling, against a real ONNX
//! session — the the parts of the model host that had no tests at all.
//!
//! Runs against the committed 285-byte fixture, so it needs no network and no
//! 528 MB download. It does need an ONNX Runtime on `ORT_DYLIB_PATH`
//! (`scripts/fetch-onnxruntime.sh`), which CI provisions.

mod common;

use common::{digest, fixture_config, fixture_with};
use onnx_scanner::runtime::config::Checksum;
use onnx_scanner::runtime::session::{LoadError, LoadedModel};
use onnx_scanner::runtime::tokenizer::{LoadedTokenizer, TokenizerLoadError};
use onnx_scanner::runtime::{
    ModelFailureReason, ModelIntegrity, NamedTensor, RuntimeError, TensorData,
};

fn inputs(ids: Vec<i64>, weights: Vec<f32>) -> Vec<NamedTensor> {
    let n = ids.len();
    vec![
        NamedTensor::new("ids", vec![1, n], TensorData::I64(ids)),
        NamedTensor::new("weights", vec![1, n], TensorData::F32(weights)),
    ]
}

// --- Load path -------------------------------------------------------------

#[tokio::test]
async fn loads_and_verifies_the_real_checksum() {
    // `skip_checksum: false` — the digest in the config is the fixture's real
    // one, so this exercises verification rather than bypassing it.
    let model = LoadedModel::load(&fixture_config())
        .await
        .expect("fixture loads");
    let snapshot = model.snapshot();
    let loaded = snapshot.loaded().expect("a loaded model reports details");

    assert_eq!(loaded.input_names, vec!["ids", "weights"]);
    assert_eq!(loaded.output_names, vec!["ids_out", "scores"]);
    assert_eq!(
        loaded.integrity,
        ModelIntegrity::Verified {
            sha256: common::FIXTURE_SHA256.to_owned()
        }
    );
    assert_eq!(loaded.replicas, 1);
}

#[tokio::test]
async fn checksum_mismatch_is_classified() {
    let Err(LoadError::Failed(failure)) = LoadedModel::load(&fixture_with(|c| {
        c.checksum = Checksum::Sha256(digest(&"0".repeat(64)));
    }))
    .await
    else {
        panic!("a wrong digest must not load");
    };
    assert_eq!(failure.reason, ModelFailureReason::ChecksumMismatch);
}

/// `skip_checksum` bypasses verification — and says so: the loaded model
/// claims no digest, rather than reporting one nobody checked.
#[tokio::test]
async fn skip_checksum_is_honoured_and_reported_unverified() {
    let cfg = fixture_with(|c| c.checksum = Checksum::Skip);
    let model = LoadedModel::load(&cfg).await.expect("loads unverified");
    assert_eq!(model.loaded_info().integrity, ModelIntegrity::Unverified);
}

#[tokio::test]
async fn unreadable_file_is_classified() {
    let Err(LoadError::Failed(failure)) =
        LoadedModel::load(&fixture_with(|c| c.model_file = "absent.onnx".to_owned())).await
    else {
        panic!("a missing file must not load");
    };
    assert_eq!(failure.reason, ModelFailureReason::FileUnreadable);
}

#[tokio::test]
async fn contract_mismatch_on_inputs_is_classified() {
    let cfg = fixture_with(|c| c.expected_inputs = vec!["not_an_input".to_owned()]);
    let Err(LoadError::Failed(failure)) = LoadedModel::load(&cfg).await else {
        panic!("a wrong input contract must not load");
    };
    assert_eq!(failure.reason, ModelFailureReason::ContractMismatch);
}

#[tokio::test]
async fn contract_mismatch_on_outputs_is_classified() {
    let cfg = fixture_with(|c| {
        c.expected_outputs = vec!["not_an_output".to_owned()];
        c.output_dtypes = [(
            "not_an_output".to_owned(),
            onnx_scanner::runtime::config::OutputDtype::F32,
        )]
        .into_iter()
        .collect();
    });
    let Err(LoadError::Failed(failure)) = LoadedModel::load(&cfg).await else {
        panic!("a wrong output contract must not load");
    };
    assert_eq!(failure.reason, ModelFailureReason::ContractMismatch);
}

/// The correctness bug this gear had: declaring a dtype the graph does not
/// emit. It must fail at load, not hand a caller reinterpreted numbers.
#[tokio::test]
async fn declared_dtype_disagreeing_with_the_graph_fails_at_load() {
    let cfg = fixture_with(|c| {
        // `scores` is f32 in the graph.
        c.output_dtypes.insert(
            "scores".to_owned(),
            onnx_scanner::runtime::config::OutputDtype::I64,
        );
    });
    let Err(LoadError::Failed(failure)) = LoadedModel::load(&cfg).await else {
        panic!("a dtype lie must not load");
    };
    assert_eq!(failure.reason, ModelFailureReason::ContractMismatch);
    let message = failure.to_string();
    assert!(message.contains("scores"), "names the output: {message}");
}

#[tokio::test]
async fn output_missing_a_declared_dtype_is_an_invalid_config() {
    let cfg = fixture_with(|c| {
        c.output_dtypes.remove("scores");
    });
    let Err(LoadError::Failed(failure)) = LoadedModel::load(&cfg).await else {
        panic!("an incomplete config must not load");
    };
    assert_eq!(failure.reason, ModelFailureReason::InvalidConfig);
}

#[test]
fn missing_tokenizer_is_classified() {
    let path = common::fixtures_dir().join("absent-tokenizer.json");
    assert!(
        matches!(
            LoadedTokenizer::load(&path),
            Err(TokenizerLoadError::Io { .. })
        ),
        "a missing tokenizer is an I/O failure"
    );
}

// --- Inference path --------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round_trips_both_dtypes_exactly() {
    let model = LoadedModel::load(&fixture_config()).await.expect("loads");

    let ids = vec![7_i64, 8, 9];
    let weights = vec![0.5_f32, -1.5, 2.25];
    let out = model
        .run(inputs(ids.clone(), weights.clone()))
        .await
        .expect("inference succeeds");

    assert_eq!(out.len(), 2, "two declared outputs come back");

    // The graph is `Identity`, so equality is exact on every platform — no
    // tolerance to choose, and any drift is a real failure.
    let by_name = |name: &str| {
        out.iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} missing from outputs"))
    };
    assert_eq!(by_name("ids_out").data, TensorData::I64(ids));
    assert_eq!(by_name("scores").data, TensorData::F32(weights));
    assert_eq!(by_name("ids_out").shape, vec![1, 3]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_shape_is_rejected_before_inference() {
    let model = LoadedModel::load(&fixture_config()).await.expect("loads");

    let bad = vec![NamedTensor::new(
        "ids",
        vec![2, 3],
        TensorData::I64(vec![1; 5]),
    )];
    let err = model
        .run(bad)
        .await
        .expect_err("a bad shape must be rejected");
    assert!(
        matches!(err, RuntimeError::InvalidInput { .. }),
        "got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_declared_input_is_a_contract_error() {
    let model = LoadedModel::load(&fixture_config()).await.expect("loads");

    // `weights` omitted.
    let partial = vec![NamedTensor::new(
        "ids",
        vec![1, 2],
        TensorData::I64(vec![1, 2]),
    )];
    let err = model
        .run(partial)
        .await
        .expect_err("an incomplete call must fail");
    assert!(
        matches!(err, RuntimeError::ContractMismatch { .. }),
        "got {err:?}"
    );
}

/// Every replica in a pool must be able to serve, not just the first. Each
/// replica is made the only free one in turn — the others held — and must
/// answer a call itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_replica_in_a_pool_serves() {
    let model = LoadedModel::load(&fixture_with(|c| c.replicas = 3))
        .await
        .expect("loads");
    assert_eq!(model.live_replicas(), 3);

    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(model.hold_replica().await.expect("a free replica"));
    }
    let mut served = std::collections::BTreeSet::new();
    for turn in 0..3 {
        // Free exactly one replica; the call can only be served by it.
        let freed = held.remove(0);
        let index = freed.replica();
        drop(freed);
        let value = i64::from(turn);
        let out = model
            .run(inputs(vec![value], vec![0.5]))
            .await
            .unwrap_or_else(|e| panic!("replica {index} must serve: {e}"));
        assert_eq!(out[0].data, TensorData::I64(vec![value]));
        served.insert(index);
        held.push(model.hold_replica().await.expect("the replica comes back"));
    }
    assert_eq!(served.len(), 3, "each of the three replicas served a call");
    drop(held);
    assert_eq!(model.live_replicas(), 3, "the pool is intact afterwards");
}

#[test]
fn the_fixture_stays_small() {
    let total: u64 = std::fs::read_dir(common::fixtures_dir())
        .expect("fixtures dir exists")
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .filter(std::fs::Metadata::is_file)
        .map(|m| m.len())
        .sum();

    assert!(
        total < 100 * 1024,
        "committed fixtures total {total} bytes; the point of a fixture is that it is small"
    );
}
