use std::collections::BTreeMap;

use async_trait::async_trait;
use object_storage_sdk::StoredObject;
use sea_orm::{ColumnTrait, Condition, EntityTrait, Set};
use toolkit_db::DbError;
use toolkit_db::secure::{
    DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, secure_insert, secure_update_with_scope,
};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repo::ObjectsRepository;

use super::entity::{self, Entity as ObjectEntity};

fn map_scope_error(e: ScopeError) -> DomainError {
    match e {
        ScopeError::Denied(msg) => DomainError::Forbidden(msg.to_owned()),
        ScopeError::Db(e) => DomainError::Database(DbError::Sea(e)),
        ScopeError::TenantNotInScope { tenant_id } => {
            DomainError::Forbidden(format!("tenant {tenant_id} not in scope"))
        }
        other => DomainError::Internal(format!("scope invalid: {other}")),
    }
}

fn to_model(row: entity::Model) -> Result<StoredObject, DomainError> {
    let labels: BTreeMap<String, String> = serde_json::from_str(&row.labels)
        .map_err(|e| DomainError::Internal(format!("object {} has invalid labels: {e}", row.id)))?;
    Ok(StoredObject {
        id: row.id,
        tenant_id: row.tenant_id,
        owner_id: row.owner_id,
        name: row.name,
        content_type: row.content_type,
        size_bytes: row.size_bytes,
        labels,
        body: row.body,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn to_active(object: &StoredObject) -> Result<entity::ActiveModel, DomainError> {
    let labels = serde_json::to_string(&object.labels)
        .map_err(|e| DomainError::Internal(format!("labels: {e}")))?;
    Ok(entity::ActiveModel {
        id: Set(object.id),
        tenant_id: Set(object.tenant_id),
        owner_id: Set(object.owner_id),
        name: Set(object.name.clone()),
        content_type: Set(object.content_type.clone()),
        size_bytes: Set(object.size_bytes),
        labels: Set(labels),
        body: Set(object.body.clone()),
        created_at: Set(object.created_at),
        updated_at: Set(object.updated_at),
    })
}

#[derive(Default)]
pub struct SeaOrmObjectsRepository;

impl SeaOrmObjectsRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ObjectsRepository for SeaOrmObjectsRepository {
    async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        object: StoredObject,
    ) -> Result<StoredObject, DomainError> {
        secure_insert::<ObjectEntity>(to_active(&object)?, scope, conn)
            .await
            .map_err(map_scope_error)?;
        Ok(object)
    }

    async fn find<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<StoredObject>, DomainError> {
        ObjectEntity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(entity::Column::Id.eq(id)))
            .one(conn)
            .await
            .map_err(map_scope_error)?
            .map(to_model)
            .transpose()
    }

    async fn update<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        object: StoredObject,
    ) -> Result<StoredObject, DomainError> {
        secure_update_with_scope::<ObjectEntity>(to_active(&object)?, scope, object.id, conn)
            .await
            .map_err(map_scope_error)?;
        Ok(object)
    }

    async fn delete<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<bool, DomainError> {
        let result = ObjectEntity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(entity::Column::Id.eq(id)))
            .exec(conn)
            .await
            .map_err(map_scope_error)?;
        Ok(result.rows_affected > 0)
    }

    async fn count_by_owner<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        owner_id: Uuid,
    ) -> Result<u64, DomainError> {
        ObjectEntity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(entity::Column::TenantId.eq(tenant_id))
                    .add(entity::Column::OwnerId.eq(owner_id)),
            )
            .count(conn)
            .await
            .map_err(map_scope_error)
    }
}
