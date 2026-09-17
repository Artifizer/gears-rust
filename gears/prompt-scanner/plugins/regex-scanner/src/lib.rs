//! Regex Scanner Plugin
//!
//! Deterministic `ScannerBackendClient` implementation for `prompt-scanner`:
//! regex + validator (Luhn, IBAN mod-97-10, `std::net` IP parsing, JWT
//! structural check, Shannon entropy) detection of PII and secret shapes.
//! No ONNX/ML dependency: deterministic detection is near-zero cost and
//! language-invariant, which no ML model in this platform replaces.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;

pub use gear::RegexScanner;
