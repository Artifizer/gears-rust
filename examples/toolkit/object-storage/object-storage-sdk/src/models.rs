//! Transport-agnostic models of the object-storage gear.

use std::collections::BTreeMap;

use time::OffsetDateTime;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// A stored object: metadata plus a small inline body.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// The subject that created the object.
    pub owner_id: Uuid,
    pub name: String,
    pub content_type: String,
    /// Size of `body` in bytes; computed by the gear, never sent by clients.
    pub size_bytes: i64,
    pub labels: BTreeMap<String, String>,
    pub body: String,
    /// Set by the gear when the object is created.
    pub created_at: OffsetDateTime,
    /// Set by the gear on every change.
    pub updated_at: OffsetDateTime,
}

/// Data to create an object.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewObject {
    pub name: String,
    pub content_type: String,
    pub labels: BTreeMap<String, String>,
    pub body: String,
}

/// A partial update; absent fields are kept.
#[domain_model]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ObjectPatch {
    pub name: Option<String>,
    pub content_type: Option<String>,
    pub labels: Option<BTreeMap<String, String>>,
    pub body: Option<String>,
}
