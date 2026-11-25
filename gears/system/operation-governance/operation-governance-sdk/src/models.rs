//! Requests and results of a Governance Check.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A set of Fact IDs.
pub type FactSet = BTreeSet<String>;

/// Whether a check may have side effects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckMode {
    /// A real check before the operation runs.
    #[default]
    Enforce,
    /// "What would happen?" — changes nothing.
    Preview,
}

/// The target resource of a governed operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernedResource {
    /// GTS type ID or stable name of the resource type.
    pub resource_type: String,
    /// Absent for an operation that creates the resource.
    pub id: Option<String>,
    /// Tenant that owns the resource.
    pub tenant_id: Uuid,
    /// Resource Version the check refers to, when the gear versions resources.
    pub version: Option<String>,
}

impl GovernedResource {
    #[must_use]
    pub fn new(resource_type: impl Into<String>, tenant_id: Uuid) -> Self {
        Self {
            resource_type: resource_type.into(),
            id: None,
            tenant_id,
            version: None,
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }
}

/// One Governance Check sent to the governance backend.
///
/// The subject is never part of the request: the backend takes it from the
/// `SecurityContext`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GovernanceCheckRequest {
    /// Governed Operation type ID (a type derived from the Governed Operation base).
    pub operation: String,
    pub resource: GovernedResource,
    /// Fact values keyed by Fact ID; only the Facts that are read.
    pub facts: serde_json::Map<String, serde_json::Value>,
    /// Facts the gear tried and failed to load, with the reason.
    pub unavailable: BTreeMap<String, String>,
    pub mode: CheckMode,
    /// 0 for the first call of an attempt; 1 after a missing-Facts answer.
    pub round: u8,
}

/// What a backend answers to one check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CheckResponse {
    /// The check is decided.
    Decided(GovernanceOutcome),
    /// The current policies need Facts the request did not carry. Internal to
    /// the SDK: the Enforcer loads them and checks once more.
    NeedFacts { missing: FactSet },
}

/// Which policy produced a refusal, and the message its author wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyFinding {
    pub policy: String,
    pub message: String,
}

/// The one combined result a gear gets for a governed operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GovernanceOutcome {
    /// The operation may run.
    Allowed,
    /// A guardrail policy blocks the operation.
    Blocked { findings: Vec<PolicyFinding> },
    /// The operation needs approval before it may run.
    ApprovalRequired { findings: Vec<PolicyFinding> },
    /// The operation type or the Facts are unknown, undeclared, or malformed.
    Invalid { message: String },
    /// A required check cannot give a reliable answer. Never treated as Allowed.
    Unavailable { message: String, retryable: bool },
}

impl GovernanceOutcome {
    /// Only [`GovernanceOutcome::Allowed`] lets the operation run.
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// Whether any policy governs an operation for a tenant, and which Facts the
/// applicable policies read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applicability {
    /// `false` lets the operation run with no check and no Fact loading.
    pub governed: bool,
    /// Fact IDs the applicable policies read.
    pub facts_read: FactSet,
    /// How long, in milliseconds, the caller may reuse this answer before it
    /// asks again: the grant lease. `0` means do not cache. A policy change
    /// reaches a caller at the latest when its lease ends.
    #[serde(default)]
    pub lease_ms: u64,
}

/// The operation family: the type ID without the version of its last segment.
///
/// Policies match by family, so a new version of an operation type keeps
/// matching the policies written for the previous one.
///
/// ```
/// use operation_governance_sdk::operation_family;
/// assert_eq!(
///     operation_family("gts.cf.core.og.governed_operation.v1~cf.demo.object.create.v2~"),
///     "gts.cf.core.og.governed_operation.v1~cf.demo.object.create~"
/// );
/// ```
#[must_use]
pub fn operation_family(type_id: &str) -> String {
    let Some(body) = type_id.strip_suffix('~') else {
        return type_id.to_owned();
    };
    let (prefix, last) = match body.rfind('~') {
        Some(i) => (&body[..=i], &body[i + 1..]),
        None => ("", body),
    };
    let segments: Vec<&str> = last.split('.').collect();
    let version_at = segments.iter().position(|s| is_version(s));
    let kept = version_at.map_or(&segments[..], |i| &segments[..i]);
    format!("{prefix}{}~", kept.join("."))
}

fn is_version(segment: &str) -> bool {
    segment
        .strip_prefix('v')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::operation_family;

    #[test]
    fn family_strips_major_and_minor_of_last_segment() {
        assert_eq!(
            operation_family("gts.cf.core.og.governed_operation.v1~cf.demo.object.create.v1.2~"),
            "gts.cf.core.og.governed_operation.v1~cf.demo.object.create~"
        );
    }

    #[test]
    fn family_of_unversioned_id_is_unchanged() {
        assert_eq!(operation_family("a.b~c.d~"), "a.b~c.d~");
        assert_eq!(operation_family("plain"), "plain");
    }
}
