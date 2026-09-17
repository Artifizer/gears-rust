//! `SeaORM`-backed implementation of [`EvidenceRepository`].

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::sea_query::{Expr, ExprTrait, Query};
use sea_orm::{ColumnTrait, Condition, EntityTrait, IntoActiveModel, Order};
use toolkit_db::secure::{DBRunner, SecureDeleteExt, SecureEntityExt, secure_insert_many};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::repos::{EvidenceMaintenance, EvidenceRepository, EvidenceRow};
use crate::infra::storage::entity::evidence::{Column, Entity};
use crate::infra::storage::mapper;

/// `SeaORM`-backed implementation of [`EvidenceRepository`] and
/// [`EvidenceMaintenance`].
#[derive(Clone, Copy, Debug, Default)]
pub struct OrmEvidenceRepository;

impl OrmEvidenceRepository {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// `(trace_id, ordinal) IN (SELECT trace_id, ordinal FROM evidences WHERE
/// <filter> LIMIT <limit>)`: a bounded batch for a maintenance delete. The
/// subquery bounds each statement, so one run never holds locks on more
/// than `limit` rows.
fn batch_of(filter: Condition, limit: u64) -> Condition {
    let batch = Query::select()
        .column(Column::TraceId)
        .column(Column::Ordinal)
        .from(Entity)
        .cond_where(filter)
        .limit(limit)
        .to_owned();
    Condition::all().add(
        Expr::tuple([Expr::col(Column::TraceId), Expr::col(Column::Ordinal)]).in_subquery(batch),
    )
}

#[async_trait]
impl EvidenceRepository for OrmEvidenceRepository {
    async fn insert_all<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        rows: Vec<EvidenceRow>,
    ) -> Result<(), DomainError> {
        let models = rows
            .into_iter()
            .map(|r| mapper::to_model(r).map(IntoActiveModel::into_active_model))
            .collect::<Result<Vec<_>, _>>()?;
        secure_insert_many::<Entity>(models, scope, runner)
            .await
            .map_err(DomainError::from)
    }

    async fn find_by_trace<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        trace_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Vec<EvidenceRow>, DomainError> {
        Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(Column::TraceId.eq(trace_id))
                    .add(Column::ExpiresAt.gt(now)),
            )
            .order_by(Column::Ordinal, Order::Asc)
            .all(runner)
            .await?
            .into_iter()
            .map(mapper::from_model)
            .collect()
    }

    async fn find_by_correlation<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        correlation_id: &str,
        now: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<EvidenceRow>, DomainError> {
        Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(Column::CorrelationId.eq(correlation_id))
                    .add(Column::ExpiresAt.gt(now)),
            )
            // UUID v7 sorts by creation time.
            .order_by(Column::TraceId, Order::Desc)
            .order_by(Column::Ordinal, Order::Asc)
            .limit(limit)
            .all(runner)
            .await?
            .into_iter()
            .map(mapper::from_model)
            .collect()
    }

    async fn delete_by_trace<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        trace_id: Uuid,
    ) -> Result<u64, DomainError> {
        let res = Entity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(Column::TraceId.eq(trace_id)))
            .exec(runner)
            .await?;
        Ok(res.rows_affected)
    }
}

#[async_trait]
impl EvidenceMaintenance for OrmEvidenceRepository {
    async fn delete_expired<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        now: DateTime<Utc>,
        limit: u64,
    ) -> Result<u64, DomainError> {
        let res = Entity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(batch_of(
                Condition::all().add(Column::ExpiresAt.lte(now)),
                limit,
            ))
            .exec(runner)
            .await?;
        Ok(res.rows_affected)
    }

    async fn count_expired<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        now: DateTime<Utc>,
    ) -> Result<u64, DomainError> {
        Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(Column::ExpiresAt.lte(now)))
            .count(runner)
            .await
            .map_err(DomainError::from)
    }

    async fn delete_tenant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        limit: u64,
    ) -> Result<u64, DomainError> {
        let res = Entity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(batch_of(
                Condition::all().add(Column::TenantId.eq(tenant_id)),
                limit,
            ))
            .exec(runner)
            .await?;
        Ok(res.rows_affected)
    }
}
