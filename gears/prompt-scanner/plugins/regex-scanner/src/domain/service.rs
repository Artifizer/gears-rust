//! Deterministic regex/validator scanning: runs the configured pattern set
//! over the payload and reports *every* match as its own `Detection` — a
//! message can legitimately contain more than one distinct piece of PII or
//! secret material, and under-reporting to "just the worst one" would mean
//! silently missing everything else in the same payload (see
//! `prompt_scanner_sdk::BackendOutcome::Detections`).
//!
//! No ML model and no async I/O: deterministic detection — near-zero cost,
//! language-invariant recall that no ML model in this platform replaces. The scan is still
//! CPU-bound, so payloads above [`INLINE_SCAN_MAX_BYTES`] run on a blocking
//! thread instead of the async executor.
//!
//! What is detected is configuration, not code: the pattern set arrives
//! already compiled from `crate::config` (see `crate::domain::pattern_set`).

use std::sync::Arc;

use async_trait::async_trait;
use prompt_scanner_sdk::{
    BackendOutcome, Detection, PromptScannerError, ScanInput, ScannerBackendClient, Severity,
    VerdictAction,
};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use tracing::{error, warn};

use crate::config::Tier;
use crate::domain::pattern_set::{Match, PatternSet, ScanOutput};

/// Largest payload scanned directly on the async executor. Bigger payloads go
/// to `tokio::task::spawn_blocking` so one large prompt cannot stall other
/// tasks on the same worker thread.
pub const INLINE_SCAN_MAX_BYTES: usize = 16 * 1024;

/// Category of the span-less detection reported when a scan could not report
/// every match (see [`ScanOutput::incomplete`]).
pub const INCOMPLETE_SCAN_CATEGORY: &str = "regex_scanner.scan_incomplete";

/// Severity corresponding to a suggested action — kept consistent across
/// backends so a tenant's thresholds behave predictably regardless of which
/// backend produced a finding.
const fn severity_for_action(action: VerdictAction) -> Severity {
    match action {
        VerdictAction::Block => Severity::Critical,
        VerdictAction::Redact => Severity::High,
        VerdictAction::Warn => Severity::Medium,
        VerdictAction::Audit | VerdictAction::Allow => Severity::Low,
    }
}

/// prompt-scanner stamps the backend's identity and `version()` on it.
fn match_to_detection(m: Match) -> Detection {
    Detection::new(
        m.category,
        severity_for_action(m.suggested_action),
        m.tier.confidence(),
        m.suggested_action,
    )
    .with_span(m.span)
}

fn to_outcome(output: ScanOutput, payload_len: usize) -> BackendOutcome {
    let mut detections: Vec<Detection> =
        output.matches.into_iter().map(match_to_detection).collect();
    if let Some(action) = output.incomplete {
        warn!(
            reported = detections.len(),
            payload_len,
            action = ?action,
            "regex-scanner did not report every match; adding a span-less detection so the \
             unreported material is handled as a whole"
        );
        // No span on purpose: a span-less `Redact` makes the gateway redact
        // the whole payload, a `Block` blocks it — either way nothing that
        // went unreported passes through untouched.
        detections.push(Detection::new(
            INCOMPLETE_SCAN_CATEGORY,
            severity_for_action(action),
            Tier::Pattern.confidence(),
            action,
        ));
    }
    BackendOutcome::Detections(detections)
}

#[domain_model]
pub struct Service {
    /// Shared with `spawn_blocking` closures, which need an owned `'static`
    /// handle.
    patterns: Arc<PatternSet>,
}

impl Service {
    #[must_use]
    pub const fn new(patterns: Arc<PatternSet>) -> Self {
        Self { patterns }
    }
}

#[async_trait]
impl ScannerBackendClient for Service {
    async fn detect(
        &self,
        _ctx: &SecurityContext,
        input: &ScanInput,
    ) -> Result<BackendOutcome, PromptScannerError> {
        let payload_len = input.payload.len();
        let output = if payload_len <= INLINE_SCAN_MAX_BYTES {
            self.patterns.scan(&input.payload)
        } else {
            let patterns = Arc::clone(&self.patterns);
            let payload = input.payload.clone();
            tokio::task::spawn_blocking(move || patterns.scan(&payload))
                .await
                .map_err(|e| {
                    error!(error = %e, payload_len, "regex-scanner blocking scan task failed");
                    PromptScannerError::internal("regex-scanner scan task failed")
                })?
        };

        Ok(to_outcome(output, payload_len))
    }

    fn version(&self) -> Option<String> {
        Some(env!("CARGO_PKG_VERSION").to_owned())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::num::NonZeroUsize;

    use prompt_scanner_sdk::{ContentKind, Span};
    use uuid::Uuid;

    use super::*;
    use crate::config::{PatternConfig, default_patterns};

    fn ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(Uuid::new_v4())
            .build()
            .expect("valid SecurityContext")
    }

    fn pattern(category: &str, regex: &str, action: VerdictAction) -> PatternConfig {
        PatternConfig {
            category: category.to_owned(),
            regex: regex.to_owned(),
            tier: Tier::Structural,
            action,
            validator: None,
            capture: 0,
        }
    }

    async fn detect(service: &Service, payload: &str) -> Vec<Detection> {
        let input = ScanInput::new(Uuid::nil(), ContentKind::Prompt, payload);
        match service.detect(&ctx(), &input).await.expect("detect") {
            BackendOutcome::Detections(d) => d,
            BackendOutcome::Defer => panic!("regex-scanner never defers"),
        }
    }

    fn service(patterns: &[PatternConfig]) -> Service {
        Service::new(Arc::new(
            PatternSet::compile(patterns, 3.0).expect("compile"),
        ))
    }

    #[tokio::test]
    async fn severity_follows_the_suggested_action() {
        let svc = service(&[
            pattern("t.block", r"\bBLK\b", VerdictAction::Block),
            pattern("t.redact", r"\bRED\b", VerdictAction::Redact),
            pattern("t.warn", r"\bWRN\b", VerdictAction::Warn),
            pattern("t.audit", r"\bAUD\b", VerdictAction::Audit),
            pattern("t.allow", r"\bALW\b", VerdictAction::Allow),
        ]);
        let mut got: Vec<(String, Severity, VerdictAction)> = detect(&svc, "BLK RED WRN AUD ALW")
            .await
            .into_iter()
            .map(|d| (d.category, d.severity, d.suggested_action))
            .collect();
        got.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            got,
            vec![
                ("t.allow".to_owned(), Severity::Low, VerdictAction::Allow),
                ("t.audit".to_owned(), Severity::Low, VerdictAction::Audit),
                (
                    "t.block".to_owned(),
                    Severity::Critical,
                    VerdictAction::Block
                ),
                ("t.redact".to_owned(), Severity::High, VerdictAction::Redact),
                ("t.warn".to_owned(), Severity::Medium, VerdictAction::Warn),
            ]
        );
    }

    #[tokio::test]
    async fn detections_carry_span_and_tier_confidence() {
        let svc = service(&[pattern("t.word", r"\bsecret\b", VerdictAction::Redact)]);
        let d = detect(&svc, "a secret here").await;
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].span, Some(Span { start: 2, end: 8 }));
        assert!((d[0].confidence - Tier::Structural.confidence()).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn a_large_payload_is_scanned_off_the_executor_with_the_same_result() {
        let svc = Service::new(Arc::new(
            PatternSet::compile(&default_patterns(), 3.0).expect("compile"),
        ));
        let padding = "x ".repeat(INLINE_SCAN_MAX_BYTES);
        let payload = format!("{padding}mail leaked@example.com");
        assert!(payload.len() > INLINE_SCAN_MAX_BYTES);

        let d = detect(&svc, &payload).await;
        let email = d
            .iter()
            .find(|d| d.category == "pii.email")
            .expect("email found in large payload");
        let span = email.span.expect("span");
        assert_eq!(
            &payload[span.start as usize..span.end as usize],
            "leaked@example.com"
        );
    }

    #[tokio::test]
    async fn an_incomplete_scan_adds_a_spanless_detection_with_the_cut_action() {
        let set = PatternSet::compile(
            &[pattern("t.word", r"\bword\b", VerdictAction::Redact)],
            3.0,
        )
        .expect("compile")
        .with_max_matches(NonZeroUsize::MIN);
        let svc = Service::new(Arc::new(set));

        let d = detect(&svc, "word word word").await;
        assert_eq!(d.len(), 2, "one reported match plus the marker");
        let marker = d
            .iter()
            .find(|d| d.category == INCOMPLETE_SCAN_CATEGORY)
            .expect("incomplete marker");
        assert_eq!(marker.span, None);
        assert_eq!(marker.suggested_action, VerdictAction::Redact);
        assert_eq!(marker.severity, Severity::High);
    }

    #[tokio::test]
    async fn a_complete_scan_has_no_marker() {
        let svc = service(&[pattern("t.word", r"\bword\b", VerdictAction::Redact)]);
        let d = detect(&svc, "word word").await;
        assert_eq!(d.len(), 2);
        assert!(d.iter().all(|d| d.category == "t.word"));
    }
}
