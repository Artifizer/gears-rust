//! Operation Governance SDK
//!
//! Everything a Consuming Gear needs to put its operations under Operation
//! Governance, and everything a governance backend implements:
//!
//! - [`gts`] — the abstract Governed Operation base type
//!   (`gts.cf.core.og.governed_operation.v1~`) that every governed operation
//!   derives from, its per-Fact traits, and the plugin spec.
//! - [`facts`] — the Fact contract a gear fulfils on demand: [`FactProvider`],
//!   [`FactWriter`], and [`FactSchema`] (read from the derived GTS type).
//! - [`enforcer`] — the [`GovernanceEnforcer`] a gear calls from its domain
//!   service, after Authorization and before the first side effect.
//! - [`plugin_api`] — [`OperationGovernancePluginClientV1`], the contract a
//!   governance backend (for example the Rego config plugin) implements.
//!
//! ## How a gear uses it
//!
//! 1. Declare one GTS type per governed operation, derived from
//!    [`GovernedOperationV1`]. Its fields are the operation's Facts. It is a
//!    governance contract, independent of the gear's REST DTOs.
//! 2. Implement [`FactProvider`] for it. The provider fills only the Facts
//!    the Enforcer asks for, so expensive Facts are loaded only when a policy
//!    reads them.
//! 3. In the domain service, after Authorization, call
//!    [`GovernanceEnforcer::check`] and refuse unless the outcome is
//!    [`GovernanceOutcome::Allowed`].
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![forbid(unsafe_code)]

pub mod enforcer;
pub mod error;
pub mod facts;
pub mod gts;
pub mod models;
pub mod plugin_api;
pub mod typed;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use enforcer::{EnforcerConfig, GovernanceEnforcer};
pub use error::OgError;
pub use facts::{FactError, FactProvider, FactSchema, FactWriter};
pub use gts::{
    FactTier, FactTraits, GovernedOperationTraits, GovernedOperationV1,
    OperationGovernancePluginSpecV1,
};
pub use models::{
    Applicability, CheckMode, CheckResponse, FactSet, GovernanceCheckRequest, GovernanceOutcome,
    GovernedResource, PolicyFinding, operation_family,
};
pub use operation_governance_macros::GovernedFacts;
pub use plugin_api::OperationGovernancePluginClientV1;
pub use typed::{FactResultExt, FactUnavailable, FactValue, LazySource};
