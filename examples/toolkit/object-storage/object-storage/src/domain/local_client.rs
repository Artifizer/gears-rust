use std::sync::Arc;

use async_trait::async_trait;
use object_storage_sdk::{NewObject, ObjectPatch, ObjectStorageClientV1, StoredObject};
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::repo::ObjectsRepository;
use super::service::Service;

/// In-process implementation of the SDK trait, registered in `ClientHub`.
#[domain_model]
pub struct LocalClient<R: ObjectsRepository + 'static> {
    service: Arc<Service<R>>,
}

impl<R: ObjectsRepository + 'static> LocalClient<R> {
    #[must_use]
    pub fn new(service: Arc<Service<R>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl<R: ObjectsRepository + 'static> ObjectStorageClientV1 for LocalClient<R> {
    async fn create_object(
        &self,
        ctx: &SecurityContext,
        object: NewObject,
    ) -> Result<StoredObject, CanonicalError> {
        self.service
            .create_object(ctx, object)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_object(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<StoredObject, CanonicalError> {
        self.service
            .get_object(ctx, id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn update_object(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: ObjectPatch,
    ) -> Result<StoredObject, CanonicalError> {
        self.service
            .update_object(ctx, id, patch)
            .await
            .map_err(CanonicalError::from)
    }

    async fn delete_object(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), CanonicalError> {
        self.service
            .delete_object(ctx, id)
            .await
            .map_err(CanonicalError::from)
    }
}
