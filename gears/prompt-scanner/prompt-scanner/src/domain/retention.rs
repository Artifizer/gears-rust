//! Maintenance deletes that span tenants: the retention job and tenant
//! deletion. Nothing on a request path holds one of these.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use toolkit_db::LockConfig;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metrics::EvidenceMetricsPort;
use crate::domain::repos::EvidenceMaintenance;
use crate::domain::service::DbProvider;

/// Advisory-lock key that elects the one retention job per database.
const RETENTION_LOCK_KEY: &str = "evidence-retention";

/// Runs the retention job and tenant deletion over [`EvidenceMaintenance`].
#[domain_model]
pub struct EvidenceMaintenanceJobs<M>
where
    M: EvidenceMaintenance + 'static,
{
    db: Arc<DbProvider>,
    repo: Arc<M>,
    metrics: Arc<dyn EvidenceMetricsPort>,
    batch_size: u64,
}

/// The scope of every maintenance delete. Privileged on purpose, and built
/// here only: retention expires rows of every tenant, and tenant deletion
/// acts for a tenant that no longer has callers. No request path reaches
/// this type.
fn maintenance_scope() -> AccessScope {
    AccessScope::allow_all()
}

impl<M> EvidenceMaintenanceJobs<M>
where
    M: EvidenceMaintenance + 'static,
{
    #[must_use]
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<M>,
        metrics: Arc<dyn EvidenceMetricsPort>,
        batch_size: u64,
    ) -> Self {
        Self {
            db,
            repo,
            metrics,
            batch_size: batch_size.max(1),
        }
    }

    /// Delete every stored row of `tenant_id`, in batches. For the tenant
    /// deletion event. Idempotent: a repeated call deletes nothing. Stops
    /// between batches when `cancel` fires. Returns the rows deleted.
    ///
    /// # Errors
    ///
    /// A database error; the rows deleted before it stay deleted, and a
    /// repeated call continues.
    pub async fn delete_tenant_evidence(
        &self,
        tenant_id: Uuid,
        cancel: &CancellationToken,
    ) -> Result<u64, DomainError> {
        let conn = self.db.conn()?;
        let scope = maintenance_scope();
        let mut total = 0;
        while !cancel.is_cancelled() {
            let deleted = self
                .repo
                .delete_tenant(&conn, &scope, tenant_id, self.batch_size)
                .await?;
            total += deleted;
            if deleted < self.batch_size {
                break;
            }
        }
        tracing::info!(tenant_id = %tenant_id, rows = total, "deleted a deleted tenant's evidence");
        Ok(total)
    }

    /// One run of the retention job: delete expired rows in batches, then
    /// report the backlog. Runs only on the instance holding the retention
    /// advisory lock, so one job per database is active; another instance
    /// skips the run. Returns the rows deleted, `None` when skipped.
    ///
    /// # Errors
    ///
    /// A database or lock error; the next run continues.
    pub async fn run_retention(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Option<u64>, DomainError> {
        let lock_cfg = LockConfig {
            max_wait: Some(Duration::ZERO),
            max_retries: Some(0),
            ..LockConfig::default()
        };
        let Some(guard) = self
            .db
            .db()
            .try_lock(crate::gear::GEAR_NAME, RETENTION_LOCK_KEY, lock_cfg)
            .await?
        else {
            tracing::debug!("retention: another instance holds the lock; skipping");
            return Ok(None);
        };

        let result = self.delete_expired(cancel).await;
        if let Err(e) = guard.release().await {
            tracing::warn!(error = %e, "retention: failed to release the lock");
        }
        result.map(Some)
    }

    async fn delete_expired(&self, cancel: &CancellationToken) -> Result<u64, DomainError> {
        let conn = self.db.conn()?;
        let scope = maintenance_scope();
        let now = Utc::now();
        let mut total = 0;
        while !cancel.is_cancelled() {
            let deleted = self
                .repo
                .delete_expired(&conn, &scope, now, self.batch_size)
                .await?;
            total += deleted;
            self.metrics.retention_deleted(deleted);
            if deleted < self.batch_size {
                break;
            }
        }
        let backlog = self.repo.count_expired(&conn, &scope, Utc::now()).await?;
        self.metrics.retention_backlog(backlog);
        if total > 0 {
            tracing::info!(rows = total, backlog, "retention: deleted expired findings");
        }
        Ok(total)
    }
}
