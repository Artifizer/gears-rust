//! Repository traits for the prompt-scanner gear's stored findings.

use std::fmt;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use prompt_scanner_sdk::{ContentKind, Finding, VerdictAction};
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// The stored text around a finding. Scanned content: `Debug` prints its
/// length only.
#[domain_model]
#[derive(Clone, PartialEq, Eq)]
pub struct Excerpt {
    /// At most 2,048 characters and 8,192 bytes.
    pub text: String,
    /// Byte offset of `text` in the scanned text.
    pub start: u32,
}

impl fmt::Debug for Excerpt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Excerpt")
            .field(
                "text",
                &format_args!("<{} bytes redacted>", self.text.len()),
            )
            .field("start", &self.start)
            .finish()
    }
}

/// One stored finding of a flagged scan: a row of `evidences`.
///
/// The scan-level fields (tenant, connector, kind, verdict, policy version,
/// expiry) repeat on every finding row of the scan. `Debug` redacts the
/// excerpt (see [`Excerpt`]).
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceRow {
    pub trace_id: Uuid,
    /// The finding's position in the scan.
    pub ordinal: u8,
    pub tenant_id: Uuid,
    pub correlation_id: Option<String>,
    pub connector_id: String,
    pub content_kind: ContentKind,
    pub created_at: DateTime<Utc>,
    /// The row counts as deleted from this moment.
    pub expires_at: DateTime<Utc>,
    /// The scan's verdict when this row was written.
    pub action: VerdictAction,
    pub settings_schema: Option<String>,
    /// `detector` is the backend's GTS instance id.
    pub finding: Finding,
    pub excerpt: Option<Excerpt>,
}

/// Request-path repository for the `evidences` table.
///
/// Rows are append-only. Reads never return a row past its `expires_at`.
/// Every method runs through Secure ORM under the caller's PDP-derived
/// `scope`.
#[async_trait]
pub trait EvidenceRepository: Send + Sync {
    /// Insert the rows of one scan. Run it in a transaction so a scan's rows
    /// land together or not at all.
    async fn insert_all<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        rows: Vec<EvidenceRow>,
    ) -> Result<(), DomainError>;

    /// The unexpired rows of one scan, by ordinal.
    async fn find_by_trace<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        trace_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<Vec<EvidenceRow>, DomainError>;

    /// The unexpired rows whose request carried `correlation_id`, newest scan
    /// first, at most `limit` rows.
    async fn find_by_correlation<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        correlation_id: &str,
        now: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<EvidenceRow>, DomainError>;

    /// Hard-delete every row of one scan. Returns the rows deleted.
    async fn delete_by_trace<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        trace_id: Uuid,
    ) -> Result<u64, DomainError>;
}

/// Maintenance deletes that span tenants: retention and tenant deletion.
///
/// Kept apart from [`EvidenceRepository`] so no request path can reach them:
/// only `domain::retention::EvidenceMaintenanceJobs` holds one. The scope is
/// still the caller's to give; that code builds the privileged one.
#[async_trait]
pub trait EvidenceMaintenance: Send + Sync {
    /// Hard-delete at most `limit` rows with `expires_at <= now`. Returns
    /// the rows deleted.
    async fn delete_expired<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        now: DateTime<Utc>,
        limit: u64,
    ) -> Result<u64, DomainError>;

    /// Rows with `expires_at <= now` not yet deleted: the retention backlog.
    async fn count_expired<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        now: DateTime<Utc>,
    ) -> Result<u64, DomainError>;

    /// Hard-delete at most `limit` rows of `tenant_id`. Returns the rows
    /// deleted.
    async fn delete_tenant<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        tenant_id: Uuid,
        limit: u64,
    ) -> Result<u64, DomainError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_excerpt_text() {
        let excerpt = Excerpt {
            text: "AKIA-very-secret".to_owned(),
            start: 3,
        };
        let printed = format!("{excerpt:?}");
        assert!(!printed.contains("very-secret"), "{printed}");
        assert!(printed.contains("16 bytes"), "{printed}");
    }
}
