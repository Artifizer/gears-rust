//! Governed Operation types of the object-storage gear.
//!
//! One type per operation, derived from the Operation Governance base type
//! `gts.cf.core.og.governed_operation.v1~`. The type ID is the operation ID
//! policies are written for, and the fields are the Facts they may read as
//! `input.facts.<field>`.
//!
//! Each Fact is declared once, on its field, with `#[fact(...)]`.
//! `#[derive(GovernedFacts)]` turns that into:
//!
//! - the GTS traits (`traits = X::og_traits()`), never JSON by hand;
//! - a typed provider trait `XFacts`, one method per Fact, whose shape follows
//!   the tier:
//!
//! | Tier | Method | So it cannot… | Examples |
//! |---|---|---|---|
//! | `request` | `fn f(input: &Self::Input)` | await, or reach a repository | `name` on create |
//! | `computed` | `fn f(input: &Self::Input)` | await, or reach a repository | `size_bytes`, `label_count` |
//! | `costly` | `async fn f(&self)` | — (it may do I/O) | `owner_object_count`, `age_seconds` on delete |
//!
//! - a generated test that the GTS schema carries exactly these Facts and
//!   traits (a `properties = "…"` name that is not a field is already a
//!   compile error).
//!
//! `age_seconds` is built from `created_at`, which the gear generates: policies
//! can govern data that no client ever sends.
//!
//! These types are versioned on their own, apart from the REST API. Adding an
//! optional Fact is compatible; renaming one needs a new version that lists the
//! old name in `#[governed_facts(replaces(old = "new"))]`.

use std::collections::BTreeMap;

use operation_governance_sdk::{GovernedFacts, GovernedOperationV1};
use toolkit_gts::gts_type_schema;

/// GTS type of a stored object, used for authorization and governance.
/// Vendor `x` marks an example type.
pub const OBJECT_RESOURCE_TYPE: &str = toolkit_gts::gts_id!("x.object_storage.storage.object.v1~");

/// Create an object.
#[gts_type_schema(
    dir_path = "schemas",
    base = GovernedOperationV1,
    type_id = toolkit_gts::gts_id!("cf.core.og.governed_operation.v1~x.object_storage.object.create.v1~"),
    description = "object-storage: create an object",
    properties = "name,content_type,size_bytes,label_count,owner_object_count",
    traits = CreateObjectV1::og_traits()
)]
#[derive(Default, GovernedFacts)]
pub struct CreateObjectV1 {
    /// Name of the new object.
    #[fact(request)]
    pub name: String,
    /// MIME type of the new object.
    #[fact(request)]
    pub content_type: String,
    /// Size of the body in bytes.
    #[fact(computed, approver_visible)]
    pub size_bytes: u64,
    /// Number of labels.
    #[fact(computed)]
    pub label_count: u32,
    /// Objects the caller already owns in this tenant: a database count.
    #[fact(costly, approver_visible, binding = false)]
    pub owner_object_count: u64,
}

/// Read an object.
#[gts_type_schema(
    dir_path = "schemas",
    base = GovernedOperationV1,
    type_id = toolkit_gts::gts_id!("cf.core.og.governed_operation.v1~x.object_storage.object.get.v1~"),
    description = "object-storage: read an object",
    properties = "content_type,labels,age_seconds,is_owner",
    traits = GetObjectV1::og_traits()
)]
#[derive(Default, GovernedFacts)]
pub struct GetObjectV1 {
    /// MIME type of the object.
    #[fact(computed)]
    pub content_type: String,
    /// Labels of the object.
    #[fact(computed)]
    pub labels: BTreeMap<String, String>,
    /// Seconds since the gear created the object.
    #[fact(computed, binding = false)]
    pub age_seconds: i64,
    /// Whether the caller created the object.
    #[fact(computed)]
    pub is_owner: bool,
}

/// Change an object.
#[gts_type_schema(
    dir_path = "schemas",
    base = GovernedOperationV1,
    type_id = toolkit_gts::gts_id!("cf.core.og.governed_operation.v1~x.object_storage.object.update.v1~"),
    description = "object-storage: change an object",
    properties = "content_type,size_bytes,size_delta_bytes,age_seconds,is_owner",
    traits = UpdateObjectV1::og_traits()
)]
#[derive(Default, GovernedFacts)]
pub struct UpdateObjectV1 {
    /// MIME type after the change.
    #[fact(computed)]
    pub content_type: String,
    /// Size after the change.
    #[fact(computed, approver_visible)]
    pub size_bytes: u64,
    /// Size after the change minus size before it.
    #[fact(computed)]
    pub size_delta_bytes: i64,
    /// Seconds since the gear created the object.
    #[fact(computed, binding = false)]
    pub age_seconds: i64,
    /// Whether the caller created the object.
    #[fact(computed)]
    pub is_owner: bool,
}

/// Delete an object. The service deletes by ID without reading the object, so
/// every Fact here is costly, and loaded only when a policy reads it.
#[gts_type_schema(
    dir_path = "schemas",
    base = GovernedOperationV1,
    type_id = toolkit_gts::gts_id!("cf.core.og.governed_operation.v1~x.object_storage.object.delete.v1~"),
    description = "object-storage: delete an object",
    properties = "content_type,size_bytes,age_seconds,is_owner",
    traits = DeleteObjectV1::og_traits()
)]
#[derive(Default, GovernedFacts)]
pub struct DeleteObjectV1 {
    /// MIME type of the object.
    #[fact(costly)]
    pub content_type: String,
    /// Size of the object.
    #[fact(costly, approver_visible)]
    pub size_bytes: u64,
    /// Seconds since the gear created the object.
    #[fact(costly, approver_visible, binding = false)]
    pub age_seconds: i64,
    /// Whether the caller created the object.
    #[fact(costly)]
    pub is_owner: bool,
}
