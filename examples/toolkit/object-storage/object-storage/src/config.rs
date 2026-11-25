//! Gear configuration.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObjectStorageConfig {
    /// Largest object body, in bytes.
    pub max_body_bytes: usize,
    /// Vendor of the governance plugin instance the Enforcer selects.
    pub governance_vendor: String,
    /// Upper bound on the grant lease of cached governance rules, in
    /// milliseconds. The Enforcer caches an applicability answer for the lease
    /// the governance plugin grants, never longer than this.
    pub governance_max_lease_ms: u64,
}

impl Default for ObjectStorageConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: 1024 * 1024,
            governance_vendor: "constructorfabric".to_owned(),
            governance_max_lease_ms: 60_000,
        }
    }
}
