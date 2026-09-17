//! OpenTelemetry adapter for [`EvidenceMetricsPort`].
//!
//! Counter names omit a `_total` suffix: the Prometheus exporter appends it.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Meter};
use prompt_scanner_sdk::UnscoredReason;

use crate::domain::metrics::{EvidenceMetricsPort, NotStored};

/// OpenTelemetry-backed implementation of [`EvidenceMetricsPort`].
pub struct EvidenceMetricsMeter {
    scans: Counter<u64>,
    findings_not_stored: Counter<u64>,
    evidence_write_failures: Counter<u64>,
    retention_deleted: Counter<u64>,
    retention_backlog: Gauge<u64>,
    jobs_dropped: Counter<u64>,
    deferred_backend_failures: Counter<u64>,
    unscored_backends: Counter<u64>,
}

impl EvidenceMetricsMeter {
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            scans: meter
                .u64_counter("prompt_scanner_scans")
                .with_description("Completed scans (outcome = flagged | clean)")
                .build(),
            findings_not_stored: meter
                .u64_counter("prompt_scanner_findings_not_stored")
                .with_description(
                    "Findings left out of storage (reason = over_cap | invalid_value | queue_full)",
                )
                .build(),
            evidence_write_failures: meter
                .u64_counter("prompt_scanner_evidence_write_failures")
                .with_description("Flagged scans whose findings failed to store")
                .build(),
            retention_deleted: meter
                .u64_counter("prompt_scanner_retention_deleted_rows")
                .with_description("Finding rows deleted by the retention job")
                .build(),
            retention_backlog: meter
                .u64_gauge("prompt_scanner_retention_backlog_rows")
                .with_description("Finding rows past expires_at not yet deleted")
                .build(),
            jobs_dropped: meter
                .u64_counter("prompt_scanner_jobs_dropped")
                .with_description("Async jobs refused on a full queue or dropped at shutdown")
                .build(),
            deferred_backend_failures: meter
                .u64_counter("prompt_scanner_deferred_backend_failures")
                .with_description(
                    "Deferred backend calls that failed, timed out or deferred again \
                     (required = true | false)",
                )
                .build(),
            unscored_backends: meter
                .u64_counter("prompt_scanner_unscored_backends")
                .with_description(
                    "Non-required backends that did not contribute to a scan's verdict \
                     (reason = timeout | unavailable | error | deferred)",
                )
                .build(),
        }
    }
}

impl EvidenceMetricsPort for EvidenceMetricsMeter {
    fn scan_completed(&self, flagged: bool) {
        let outcome = if flagged { "flagged" } else { "clean" };
        self.scans.add(1, &[KeyValue::new("outcome", outcome)]);
    }

    fn findings_not_stored(&self, reason: NotStored, count: u64) {
        if count > 0 {
            self.findings_not_stored
                .add(count, &[KeyValue::new("reason", reason.label())]);
        }
    }

    fn evidence_write_failed(&self) {
        self.evidence_write_failures.add(1, &[]);
    }

    fn retention_deleted(&self, rows: u64) {
        if rows > 0 {
            self.retention_deleted.add(rows, &[]);
        }
    }

    fn retention_backlog(&self, rows: u64) {
        self.retention_backlog.record(rows, &[]);
    }

    fn jobs_dropped(&self, count: u64) {
        if count > 0 {
            self.jobs_dropped.add(count, &[]);
        }
    }

    fn deferred_backend_failed(&self, required: bool) {
        self.deferred_backend_failures
            .add(1, &[KeyValue::new("required", required)]);
    }

    fn backend_unscored(&self, reason: UnscoredReason) {
        self.unscored_backends
            .add(1, &[KeyValue::new("reason", reason.as_str())]);
    }
}
