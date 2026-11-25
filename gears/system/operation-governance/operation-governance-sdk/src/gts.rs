//! GTS types of Operation Governance.
//!
//! - [`GovernedOperationV1`] — the abstract base every governed operation
//!   derives from. A derived type names one operation (by resource and action,
//!   never by route) and its fields are the operation's Facts.
//! - [`GovernedOperationTraits`] — per-Fact metadata, declared as traits of the
//!   derived type: Fact Tier, sensitivity, approver visibility, binding.
//! - [`OperationGovernancePluginSpecV1`] — the plugin spec a governance backend
//!   registers an instance of.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use toolkit_gts::{GtsTraitsSchema, PluginV1, gts_type_schema};

/// Plugin spec of an Operation Governance backend.
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.og.plugin.v1~"),
    description = "Operation Governance plugin specification",
    properties = "",
)]
pub struct OperationGovernancePluginSpecV1;

/// How costly a Fact is to get. The Enforcer loads cheaper tiers first.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FactTier {
    /// Already in the request or the security context.
    #[default]
    Request,
    /// Derived in memory.
    Computed,
    /// Needs a database read or a call to another gear.
    Costly,
}

const fn default_true() -> bool {
    true
}

/// Metadata of one Fact. The flags mirror the GTS trait JSON one to one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FactTraits {
    /// Cost tier; `request` when absent.
    #[serde(default)]
    pub tier: FactTier,
    /// Never sent to an external decision engine unless it is allowed it.
    #[serde(default)]
    pub sensitive: bool,
    /// Shown to Approvers, and loaded when approval may be required.
    #[serde(default)]
    pub approver_visible: bool,
    /// Part of the approval binding. Volatile Facts, such as counts, are not.
    #[serde(default = "default_true")]
    pub binding: bool,
}

impl Default for FactTraits {
    fn default() -> Self {
        Self {
            tier: FactTier::Request,
            sensitive: false,
            approver_visible: false,
            binding: true,
        }
    }
}

/// Traits of a Governed Operation type, emitted as the base type's
/// `x-gts-traits-schema`. Derived types fill them through `traits = json!(..)`.
#[derive(JsonSchema, Serialize, GtsTraitsSchema)]
#[serde(deny_unknown_fields)]
pub struct GovernedOperationTraits {
    /// Per-Fact metadata, keyed by Fact ID (a top-level field of `facts`).
    /// A Fact with no entry is a request-tier, non-sensitive, binding Fact.
    #[serde(default)]
    pub facts: BTreeMap<String, FactTraits>,
    /// Fact IDs of the previous type version renamed in this one
    /// (old ID → new ID), so Policies written for the old version keep matching.
    #[serde(default)]
    pub replaces: BTreeMap<String, String>,
}

fn operation_type_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let mut schema = <gts::GtsTypeId as JsonSchema>::json_schema(generator);
    schema.insert(gts::X_GTS_TYPE_REF.to_owned(), gts::SELF_ID_REF.into());
    schema
}

/// Abstract base of every Governed Operation.
///
/// A derived type's ID is the operation ID, and its fields are the Facts
/// Policies may read, as `input.facts.<id>` in Rego.
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.og.governed_operation.v1~"),
    description = "Base Governed Operation. A derived type names one governed operation; \
                   its `facts` schema declares every Fact a policy may read about it, and \
                   its traits give per-Fact metadata.",
    properties = "gts_type,facts",
    traits_schema = inline(GovernedOperationTraits),
    // Abstract bases must carry trait defaults: the GTS store rejects an
    // `x-gts-traits-schema` with no values in the derivation chain.
    traits = serde_json::json!({ "facts": {}, "replaces": {} }),
    gts_abstract = true
)]
pub struct GovernedOperationV1<F: gts::GtsSchema> {
    /// The derived Governed Operation type this check instantiates. Wire key `type`.
    #[serde(rename = "type")]
    #[schemars(schema_with = "operation_type_schema")]
    pub gts_type: gts::GtsTypeId,
    /// Facts of one check. Each top-level field is one Fact, the unit of lazy loading.
    pub facts: F,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gts_tests.rs"]
mod gts_tests;
