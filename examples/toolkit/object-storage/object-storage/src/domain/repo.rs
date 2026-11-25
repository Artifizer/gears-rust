//! Repository port.

use async_trait::async_trait;
use object_storage_sdk::StoredObject;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::error::DomainError;

#[async_trait]
pub trait ObjectsRepository: Send + Sync {
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        object: StoredObject,
    ) -> Result<StoredObject, DomainError>;

    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<StoredObject>, DomainError>;

    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        object: StoredObject,
    ) -> Result<StoredObject, DomainError>;

    /// `true` when a row was removed.
    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError>;

    /// Objects `owner_id` owns in `tenant_id`, within `scope`.
    async fn count_by_owner<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        owner_id: Uuid,
    ) -> Result<u64, DomainError>;
}
