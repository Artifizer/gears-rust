use std::collections::HashMap;
use std::time::Duration;

use prompt_scanner_sdk::{ContentKind, EvidencePolicy, TenantPolicy, VerdictAction};
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

/// What the gear does when it cannot tell which policy applies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailMode {
    /// Refuse the request with `PolicyUnavailable` (503). The default.
    #[default]
    Closed,
    /// Log a warning and carry on with the fallback (or the permissive
    /// default) policy.
    Open,
}

/// Configuration for the `prompt_scanner` gear.
///
/// Durations take `humantime` strings (`"5s"`, `"200ms"`). The old
/// millisecond keys (`backend_timeout_ms`, `job_poll_interval_ms`) are
/// accepted as aliases; a bare integer means milliseconds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptScannerConfig {
    /// What happens when the policy cannot be resolved: no provider plugin
    /// and no `fallback_policy`, a malformed provider plugin instance, or a
    /// provider plugin without a registered client. It does not cover
    /// backends: a required backend that fails always fails the scan, and a
    /// non-required one never changes the verdict (it is listed in
    /// `ScanVerdict::unscored_backends`, logged and counted).
    #[serde(default)]
    pub fail_mode: FailMode,
    /// Per-backend call timeout on the synchronous path. A backend that
    /// doesn't respond within it degrades to "unscored" (listed in the
    /// verdict's `unscored_backends`, reason `timeout`) unless the tenant
    /// policy marks it `required`, in which case the scan fails with
    /// `BackendTimeout` (504).
    #[serde(
        default = "default_backend_timeout",
        alias = "backend_timeout_ms",
        deserialize_with = "duration_or_millis",
        serialize_with = "humantime_serde::serialize"
    )]
    pub backend_timeout: Duration,
    /// Timeout for a deferred backend's background call.
    #[serde(
        default = "default_deferred_backend_timeout",
        deserialize_with = "duration_or_millis",
        serialize_with = "humantime_serde::serialize"
    )]
    pub deferred_backend_timeout: Duration,
    /// Timeout for extracting text from an attachment via `file-parser`.
    #[serde(
        default = "default_attachment_parse_timeout",
        deserialize_with = "duration_or_millis",
        serialize_with = "humantime_serde::serialize"
    )]
    pub attachment_parse_timeout: Duration,
    /// How often the background worker takes jobs off the async queue.
    #[serde(
        default = "default_job_poll_interval",
        alias = "job_poll_interval_ms",
        deserialize_with = "duration_or_millis",
        serialize_with = "humantime_serde::serialize"
    )]
    pub job_poll_interval: Duration,
    /// Most jobs the worker runs per tick; the rest wait for the next one.
    #[serde(default = "default_jobs_per_tick")]
    pub jobs_per_tick: usize,
    /// Capacity of the in-memory async job queue. A full queue refuses
    /// `scan_async` with `Overloaded` (503).
    #[serde(default = "default_job_queue_capacity")]
    pub job_queue_capacity: usize,
    /// How long a finished job's status stays queryable.
    #[serde(
        default = "default_job_status_ttl",
        deserialize_with = "duration_or_millis",
        serialize_with = "humantime_serde::serialize"
    )]
    pub job_status_ttl: Duration,
    /// The policy every tenant gets when no `PolicyProvider` plugin is
    /// registered. Unset: see `fail_mode`.
    #[serde(default)]
    pub fallback_policy: Option<FallbackPolicy>,
    /// Stored findings and evidence: retention ceilings, the excerpt window
    /// and the retention job.
    #[serde(default)]
    pub evidence: EvidenceConfig,
}

/// Highest `max_findings_stored`: the `ordinal` column's `CHECK` allows
/// 0..=255.
pub const MAX_FINDINGS_STORED_LIMIT: u16 = 256;

/// Upper bound for the synchronous backend timeout.
const MAX_BACKEND_TIMEOUT: Duration = Duration::from_mins(5);
/// Upper bound for the background timeouts.
const MAX_BACKGROUND_TIMEOUT: Duration = Duration::from_hours(1);
/// Upper bound for any queue capacity or batch size.
const MAX_QUEUE_CAPACITY: usize = 1_000_000;
/// Upper bound for `evidence_context_chars`: an excerpt is at most this many
/// characters anyway.
const MAX_CONTEXT_CHARS: usize = crate::domain::evidence::EXCERPT_MAX_CHARS;

/// How flagged scans are stored and expired. See the `evidences` table in
/// `infra::storage::migrations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceConfig {
    /// Ceiling for any finding row's lifetime. A tenant policy's `retention`
    /// is clamped to it; an absent one means this value.
    #[serde(default = "default_max_retention", with = "humantime_serde")]
    pub max_retention: Duration,
    /// Ceiling for a finding row that holds an excerpt. At most
    /// `max_retention`.
    #[serde(default = "default_max_evidence_retention", with = "humantime_serde")]
    pub max_evidence_retention: Duration,
    /// Characters of context kept on each side of a finding's span.
    #[serde(default = "default_evidence_context_chars")]
    pub evidence_context_chars: usize,
    /// Finding rows stored per scan; the rest are counted, not stored.
    /// At most [`MAX_FINDINGS_STORED_LIMIT`].
    #[serde(default = "default_max_findings_stored")]
    pub max_findings_stored: u16,
    /// How often the retention job deletes expired rows.
    #[serde(default = "default_retention_interval", with = "humantime_serde")]
    pub retention_interval: Duration,
    /// Rows per retention `DELETE` batch.
    #[serde(default = "default_retention_batch_size")]
    pub retention_batch_size: u64,
    /// Flagged scans waiting to be written. A full queue drops the scan's
    /// rows and counts them.
    #[serde(default = "default_write_queue_capacity")]
    pub write_queue_capacity: usize,
    /// Timeout for one scan's insert transaction.
    #[serde(default = "default_write_timeout", with = "humantime_serde")]
    pub write_timeout: Duration,
}

impl Default for EvidenceConfig {
    fn default() -> Self {
        Self {
            max_retention: default_max_retention(),
            max_evidence_retention: default_max_evidence_retention(),
            evidence_context_chars: default_evidence_context_chars(),
            max_findings_stored: default_max_findings_stored(),
            retention_interval: default_retention_interval(),
            retention_batch_size: default_retention_batch_size(),
            write_queue_capacity: default_write_queue_capacity(),
            write_timeout: default_write_timeout(),
        }
    }
}

/// A config value the gear cannot run with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct InvalidConfig {
    /// The offending key, dotted (`evidence.max_retention`).
    pub field: &'static str,
    pub message: String,
}

fn invalid(field: &'static str, message: impl Into<String>) -> InvalidConfig {
    InvalidConfig {
        field,
        message: message.into(),
    }
}

fn check_duration(
    field: &'static str,
    value: Duration,
    max: Duration,
) -> Result<(), InvalidConfig> {
    if value.is_zero() {
        return Err(invalid(field, "must be greater than zero"));
    }
    if value > max {
        return Err(invalid(field, format!("must be at most {max:?}")));
    }
    Ok(())
}

fn check_count(field: &'static str, value: usize) -> Result<(), InvalidConfig> {
    if value == 0 || value > MAX_QUEUE_CAPACITY {
        return Err(invalid(
            field,
            format!("must be in 1..={MAX_QUEUE_CAPACITY}"),
        ));
    }
    Ok(())
}

impl EvidenceConfig {
    /// Reject values the schema or the retention job cannot honour.
    ///
    /// # Errors
    ///
    /// The first invalid setting.
    pub fn validate(&self) -> Result<(), InvalidConfig> {
        const FOREVER: Duration = Duration::from_hours(24 * 365 * 100);
        check_duration("evidence.max_retention", self.max_retention, FOREVER)?;
        check_duration(
            "evidence.max_evidence_retention",
            self.max_evidence_retention,
            FOREVER,
        )?;
        if self.max_evidence_retention > self.max_retention {
            return Err(invalid(
                "evidence.max_evidence_retention",
                "must not exceed evidence.max_retention",
            ));
        }
        if self.evidence_context_chars > MAX_CONTEXT_CHARS {
            return Err(invalid(
                "evidence.evidence_context_chars",
                format!("must be at most {MAX_CONTEXT_CHARS}"),
            ));
        }
        if self.max_findings_stored == 0 || self.max_findings_stored > MAX_FINDINGS_STORED_LIMIT {
            return Err(invalid(
                "evidence.max_findings_stored",
                format!("must be in 1..={MAX_FINDINGS_STORED_LIMIT}"),
            ));
        }
        check_duration(
            "evidence.retention_interval",
            self.retention_interval,
            FOREVER,
        )?;
        if self.retention_batch_size == 0 || self.retention_batch_size > MAX_QUEUE_CAPACITY as u64 {
            return Err(invalid(
                "evidence.retention_batch_size",
                format!("must be in 1..={MAX_QUEUE_CAPACITY}"),
            ));
        }
        check_count("evidence.write_queue_capacity", self.write_queue_capacity)?;
        check_duration(
            "evidence.write_timeout",
            self.write_timeout,
            MAX_BACKGROUND_TIMEOUT,
        )?;
        Ok(())
    }
}

impl PromptScannerConfig {
    /// Reject every value the gear cannot run with, naming the field.
    ///
    /// The fallback policy is checked separately, with the same rules as a
    /// policy written through the API.
    ///
    /// # Errors
    ///
    /// The first invalid setting.
    pub fn validate(&self) -> Result<(), InvalidConfig> {
        check_duration("backend_timeout", self.backend_timeout, MAX_BACKEND_TIMEOUT)?;
        check_duration(
            "deferred_backend_timeout",
            self.deferred_backend_timeout,
            MAX_BACKGROUND_TIMEOUT,
        )?;
        check_duration(
            "attachment_parse_timeout",
            self.attachment_parse_timeout,
            MAX_BACKGROUND_TIMEOUT,
        )?;
        check_duration(
            "job_poll_interval",
            self.job_poll_interval,
            MAX_BACKGROUND_TIMEOUT,
        )?;
        check_count("jobs_per_tick", self.jobs_per_tick)?;
        check_count("job_queue_capacity", self.job_queue_capacity)?;
        check_duration(
            "job_status_ttl",
            self.job_status_ttl,
            Duration::from_hours(24 * 7),
        )?;
        self.evidence.validate()
    }
}

/// A `TenantPolicy` without its tenant: the same for every tenant. Field
/// meanings and defaults are `TenantPolicy`'s.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FallbackPolicy {
    #[serde(default)]
    pub enabled_backends: Vec<String>,
    #[serde(default)]
    pub required_backends: Vec<String>,
    #[serde(default)]
    pub thresholds: HashMap<String, f32>,
    #[serde(default)]
    pub default_action: VerdictAction,
    #[serde(default)]
    pub category_action_overrides: HashMap<String, VerdictAction>,
    #[serde(default = "ContentKind::all")]
    pub content_kinds: Vec<ContentKind>,
    #[serde(default, with = "humantime_serde")]
    pub retention: Option<Duration>,
    #[serde(default)]
    pub evidence: EvidencePolicy,
}

impl FallbackPolicy {
    /// As a `TenantPolicy` for the nil tenant; the provider substitutes the
    /// tenant asked for.
    #[must_use]
    pub fn into_template(self) -> TenantPolicy {
        let mut policy = TenantPolicy::default_for_tenant(Uuid::nil());
        policy.enabled_backends = self.enabled_backends;
        policy.required_backends = self.required_backends;
        policy.thresholds = self.thresholds;
        policy.default_action = self.default_action;
        policy.category_action_overrides = self.category_action_overrides;
        policy.content_kinds = self.content_kinds;
        policy.retention = self.retention;
        policy.evidence = self.evidence;
        policy
    }
}

impl Default for PromptScannerConfig {
    fn default() -> Self {
        Self {
            fail_mode: FailMode::default(),
            backend_timeout: default_backend_timeout(),
            deferred_backend_timeout: default_deferred_backend_timeout(),
            attachment_parse_timeout: default_attachment_parse_timeout(),
            job_poll_interval: default_job_poll_interval(),
            jobs_per_tick: default_jobs_per_tick(),
            job_queue_capacity: default_job_queue_capacity(),
            job_status_ttl: default_job_status_ttl(),
            fallback_policy: None,
            evidence: EvidenceConfig::default(),
        }
    }
}

/// A duration as a `humantime` string (`"5s"`), or a bare integer of
/// milliseconds (the old `*_ms` keys).
fn duration_or_millis<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Millis(u64),
        Text(String),
    }
    match Raw::deserialize(d)? {
        Raw::Millis(ms) => Ok(Duration::from_millis(ms)),
        Raw::Text(s) => humantime::parse_duration(&s).map_err(serde::de::Error::custom),
    }
}

const DAY: Duration = Duration::from_hours(24);

fn default_max_retention() -> Duration {
    DAY * 90
}

fn default_max_evidence_retention() -> Duration {
    DAY * 30
}

fn default_evidence_context_chars() -> usize {
    256
}

fn default_max_findings_stored() -> u16 {
    MAX_FINDINGS_STORED_LIMIT
}

fn default_retention_interval() -> Duration {
    Duration::from_mins(5)
}

fn default_retention_batch_size() -> u64 {
    10_000
}

fn default_write_queue_capacity() -> usize {
    1_024
}

fn default_write_timeout() -> Duration {
    Duration::from_secs(10)
}

fn default_backend_timeout() -> Duration {
    Duration::from_secs(5)
}

fn default_deferred_backend_timeout() -> Duration {
    Duration::from_secs(60)
}

fn default_attachment_parse_timeout() -> Duration {
    Duration::from_secs(30)
}

fn default_job_poll_interval() -> Duration {
    Duration::from_millis(200)
}

fn default_jobs_per_tick() -> usize {
    32
}

fn default_job_queue_capacity() -> usize {
    1_024
}

fn default_job_status_ttl() -> Duration {
    Duration::from_hours(1)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
