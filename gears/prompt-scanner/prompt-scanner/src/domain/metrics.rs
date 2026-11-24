//! Metrics the domain reports. Content-free: counts and labels only.
//!
//! A port so the domain does not import `opentelemetry`; the gear wires
//! `infra::metrics::EvidenceMetricsMeter`, tests use [`NoopMetrics`].

pub use crate::domain::evidence::NotStored;
use prompt_scanner_sdk::UnscoredReason;

/// Counters and gauges for scans, stored findings and their retention.
pub trait EvidenceMetricsPort: Send + Sync {
    /// One scan finished: `flagged` when it had findings to store. Clean
    /// scans appear here only; they are never stored.
    fn scan_completed(&self, flagged: bool);
    /// Findings left out of storage, by reason.
    fn findings_not_stored(&self, reason: NotStored, count: u64);
    /// A scan's rows failed to insert; that scan's evidence is lost.
    fn evidence_write_failed(&self);
    /// Rows the retention job deleted.
    fn retention_deleted(&self, rows: u64);
    /// Rows past `expires_at` not yet deleted.
    fn retention_backlog(&self, rows: u64);
    /// Async jobs (`scan_async` or deferred backends) refused because the
    /// queue was full, or dropped at shutdown before they ran.
    fn jobs_dropped(&self, count: u64);
    /// A deferred backend failed, timed out or deferred again in the
    /// background; `required` when the tenant policy requires it.
    fn deferred_backend_failed(&self, required: bool);
    /// A non-required backend did not contribute to a scan's verdict, for
    /// `reason`. Listed in `ScanVerdict::unscored_backends`.
    fn backend_unscored(&self, reason: UnscoredReason);
}

/// Discards everything.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;

impl EvidenceMetricsPort for NoopMetrics {
    fn scan_completed(&self, _flagged: bool) {}
    fn findings_not_stored(&self, _reason: NotStored, _count: u64) {}
    fn evidence_write_failed(&self) {}
    fn retention_deleted(&self, _rows: u64) {}
    fn retention_backlog(&self, _rows: u64) {}
    fn jobs_dropped(&self, _count: u64) {}
    fn deferred_backend_failed(&self, _required: bool) {}
    fn backend_unscored(&self, _reason: UnscoredReason) {}
}
