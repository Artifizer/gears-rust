//! ONNX Scanner Plugin
//!
//! `ScannerBackendClient` implementation for `prompt-scanner` that detects
//! PII via an ONNX model, run in-process by the plugin's own model host
//! (`runtime`: tokenize + infer only — decoding is the backends' job) on the
//! toolkit's ONNX session. Replaces the retired `pii-plugin` and the
//! standalone `onnx-runner` gear. See `docs/runtime/DESIGN.md`.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
mod domain;
/// Detection-quality evaluation tooling. Never built into the shipped plugin.
#[cfg(feature = "eval")]
pub mod eval;
/// The model host. Crate-private in a normal build; public only under the
/// `test-support` feature (enabled by this crate's own dev-dependency) so the
/// integration tests and examples can drive the real load, run and readiness
/// paths. Not public API.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod runtime;
// The model host is written as a self-contained API (its own tests drive all
// of it), and the shipped plugin uses only part of that API. Which part
// depends on the feature set (`eval` reaches more of it), so an `#[expect]`
// would be unfulfilled in some combinations; `allow` is scoped to the private
// build only.
#[cfg(not(any(test, feature = "test-support")))]
#[allow(
    dead_code,
    unused_imports,
    reason = "crate-private build of an API the tests use in full; see above"
)]
pub(crate) mod runtime;
/// Re-exported only for `eval`, whose public scoring functions take it. The
/// shipped plugin keeps `domain` private.
#[cfg(feature = "eval")]
pub use domain::entity::Entity;
pub mod gear;

pub use gear::OnnxScanner;
