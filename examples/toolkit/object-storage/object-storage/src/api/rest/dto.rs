//! REST DTOs. They are the wire contract of the API and are versioned with it;
//! the governance contract (the Governed Operation types in the SDK) is
//! separate, so changing a DTO never changes a Fact a policy reads.

use std::collections::BTreeMap;

use object_storage_sdk::{NewObject, ObjectPatch, StoredObject};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ObjectDto {
    #[schema(value_type = String)]
    pub id: Uuid,
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    #[schema(value_type = String)]
    pub owner_id: Uuid,
    pub name: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub labels: BTreeMap<String, String>,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<StoredObject> for ObjectDto {
    fn from(o: StoredObject) -> Self {
        Self {
            id: o.id,
            tenant_id: o.tenant_id,
            owner_id: o.owner_id,
            name: o.name,
            content_type: o.content_type,
            size_bytes: o.size_bytes,
            labels: o.labels,
            body: o.body,
            created_at: o.created_at,
            updated_at: o.updated_at,
        }
    }
}

#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct CreateObjectRequest {
    pub name: String,
    pub content_type: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub body: String,
}

impl From<CreateObjectRequest> for NewObject {
    fn from(r: CreateObjectRequest) -> Self {
        Self {
            name: r.name,
            content_type: r.content_type,
            labels: r.labels,
            body: r.body,
        }
    }
}

#[derive(Debug, Clone, Default)]
#[toolkit_macros::api_dto(request)]
pub struct PatchObjectRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub labels: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub body: Option<String>,
}

impl From<PatchObjectRequest> for ObjectPatch {
    fn from(r: PatchObjectRequest) -> Self {
        Self {
            name: r.name,
            content_type: r.content_type,
            labels: r.labels,
            body: r.body,
        }
    }
}
