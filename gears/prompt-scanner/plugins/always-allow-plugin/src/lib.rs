//! Always-Allow Scanner Backend Plugin
//!
//! Minimal `ScannerBackendClient` implementation for `prompt-scanner`: never
//! finds anything and reports an empty finding set. It is the smallest real
//! backend plugin — used in the demo configuration
//! (`config/gears.d/prompt-scanner.yaml`) to show that `prompt-scanner`'s
//! fan-out resolves and calls registered backends, and as a reference for
//! writing a new one. It is not a detector. See
//! `gears/prompt-scanner/docs/DESIGN.md`.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;

pub use gear::AlwaysAllowPlugin;
