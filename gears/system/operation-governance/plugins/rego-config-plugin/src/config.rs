//! Configuration of the Rego config plugin.

use serde::Deserialize;

/// Default grant lease: 30 seconds.
pub const DEFAULT_APPLICABILITY_LEASE_MS: u64 = 30_000;

/// Longest grant lease accepted: 10 minutes.
pub const MAX_APPLICABILITY_LEASE_MS: u64 = 600_000;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OgRegoConfigPluginConfig {
    /// Vendor the Governance Enforcer selects this instance by.
    pub vendor: String,
    /// Lower wins when several instances share a vendor.
    pub priority: i16,
    /// Wall-clock bound of one policy evaluation.
    pub eval_timeout_ms: u64,
    /// Grant lease of an applicability answer, in milliseconds: how long a
    /// Governance Enforcer may reuse it before asking again. It bounds how
    /// late a policy change reaches a gear (the Policy Propagation Delay);
    /// `0` turns caching off.
    pub applicability_lease_ms: u64,
    /// Governed Operations and their policies.
    pub operations: Vec<OperationPolicies>,
}

impl Default for OgRegoConfigPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 100,
            eval_timeout_ms: 50,
            applicability_lease_ms: DEFAULT_APPLICABILITY_LEASE_MS,
            operations: Vec::new(),
        }
    }
}

/// The policies of one Governed Operation.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationPolicies {
    /// Governed Operation type ID, or its family (the ID without the version of
    /// the last segment). Either way the policies apply to every version.
    pub operation: String,
    pub policies: Vec<PolicyConfig>,
}

/// One Rego policy.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    /// Name reported in refusals.
    pub name: String,
    pub kind: PolicyKind,
    /// Rego source; it defines `deny` (guardrail) or `require_approval` (approval).
    pub source: String,
}

/// What a policy's result means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyKind {
    /// Blocks the operation; entrypoint `deny`.
    Guardrail,
    /// Requires approval; entrypoint `require_approval`.
    Approval,
}

impl PolicyKind {
    /// The rule the policy must define.
    #[must_use]
    pub const fn entrypoint(self) -> &'static str {
        match self {
            Self::Guardrail => "deny",
            Self::Approval => "require_approval",
        }
    }
}
