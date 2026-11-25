//! `ObjectStorageClientV1`: the gear's public API.

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::models::{NewObject, ObjectPatch, StoredObject};

/// Public API of the object-storage gear, registered in `ClientHub`.
///
/// Every call is authorized and governed: a governance refusal is returned as
/// a canonical error like any other refusal.
#[async_trait]
pub trait ObjectStorageClientV1: Send + Sync {
    async fn create_object(
        &self,
        ctx: &SecurityContext,
        object: NewObject,
    ) -> Result<StoredObject, CanonicalError>;

    async fn get_object(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<StoredObject, CanonicalError>;

    async fn update_object(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: ObjectPatch,
    ) -> Result<StoredObject, CanonicalError>;

    async fn delete_object(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), CanonicalError>;
}
