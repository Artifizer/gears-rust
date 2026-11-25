//! Operation Governance Rego Config Plugin
//!
//! A governance backend whose policies are Rego documents written in the
//! plugin's configuration file. Each policy belongs to one Governed Operation
//! (by its type ID or family) and has a kind:
//!
//! - `guardrail` — the document defines `deny`, a set of messages. Any message
//!   blocks the operation.
//! - `approval` — the document defines `require_approval`, a set of messages.
//!   Any message makes the operation need approval. Approval policies run only
//!   when no guardrail blocks, so nobody is asked to approve what cannot run.
//!
//! Policies read Facts as `input.facts.<id>`. Each policy's reads set is
//! computed when it is compiled, at startup; the plugin tells the Enforcer to
//! load only those Facts, and asks once for any the check did not carry. A
//! policy whose Fact references cannot be resolved statically is refused at
//! startup. The input document also has `subject`, `operation`, `resource`,
//! `mode`, and the evaluation time (`evaluated_at`, `evaluated_at_ns`).
//!
//! ## Configuration
//!
//! ```yaml
//! gears:
//!   og-rego-config-plugin:
//!     config:
//!       vendor: "constructorfabric"
//!       priority: 100
//!       eval_timeout_ms: 50
//!       applicability_lease_ms: 30000   # grant lease of cached rules (default 30 s)
//!       operations:
//!         - operation: "gts.cf.core.og.governed_operation.v1~cf.example.object.delete~"
//!           policies:
//!             - name: "retention"
//!               kind: guardrail
//!               source: |
//!                 package object.retention
//!                 deny contains "objects younger than one hour cannot be deleted" if {
//!                   input.facts.age_seconds < 3600
//!                 }
//! ```
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod config;
pub mod domain;
pub mod gear;

pub use gear::OgRegoConfigPlugin;
