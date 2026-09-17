//! PII detection orchestration: input size limits, confidence-to-action
//! mapping, `Detection` construction. Model-specific detection itself lives
//! behind [`PiiBackend`] — see `domain::backend`'s docs for why, and
//! `domain::bio_backend`/`domain::gliner2` for the two implementations.
//!
//! # Oversized inputs fail closed
//!
//! A payload over `max_words` words or `max_payload_bytes` bytes is **not
//! scanned**, and `detect` says so with an error
//! ([`PromptScannerError::Validation`]) rather than a result. It never returns
//! [`BackendOutcome::Defer`].
//!
//! An error is honest: `prompt-scanner` fails the scan when this backend is
//! required by the tenant's policy, and otherwise lists it in the verdict's
//! `unscored_backends` (reason `error`; an unavailable model is reported as
//! [`PromptScannerError::BackendUnavailable`], reason `unavailable`), logs
//! and counts it, without changing the verdict action. Deferring would not
//! help: the background retry ([`ScanInput::is_deferred_run`]) has a larger
//! time budget, but the same model input limits. Chunking long inputs so
//! they can be scanned in pieces is the follow-up that would lift the limit.
//!
//! # Cancellation
//!
//! `detect` is cancel-safe in the sense that dropping its future leaves no
//! shared state half-updated: the backends hold no lock across an `.await`.
//! It does not stop work already handed to the model host — ONNX Runtime
//! cannot interrupt an inference, so a dropped call keeps its replica busy
//! until that inference finishes (bounded by the model's
//! `inference_timeout`).

use std::sync::Arc;

use async_trait::async_trait;
use prompt_scanner_sdk::{
    BackendOutcome, Detection, PromptScannerError, ScanInput, ScannerBackendClient, Severity, Span,
    VerdictAction,
};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use crate::domain::backend::PiiBackend;
use crate::domain::entity::Entity;
use crate::domain::error::DomainError;

/// Confidence cut-offs for the suggested action. Both in `[0, 1]`, and
/// `warn <= redact`, so the mapping is monotonic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    redact: f32,
    warn: f32,
}

impl Thresholds {
    /// # Errors
    ///
    /// A description of the problem when either value is not a finite number
    /// in `[0, 1]`, or `warn > redact`.
    pub fn new(redact: f32, warn: f32) -> Result<Self, String> {
        let in_range = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        if !in_range(redact) || !in_range(warn) {
            return Err(format!(
                "must both be numbers in [0, 1], got redact={redact}, warn={warn}"
            ));
        }
        if warn > redact {
            return Err(format!("warn ({warn}) must not exceed redact ({redact})"));
        }
        Ok(Self { redact, warn })
    }

    /// The verdict action a single detected entity's confidence suggests.
    ///
    /// A PII hit defaults to suggesting `Redact` rather than `Block` — same
    /// posture as the retired `pii-plugin`.
    fn action_for(self, score: f32) -> VerdictAction {
        if score >= self.redact {
            VerdictAction::Redact
        } else if score >= self.warn {
            VerdictAction::Warn
        } else {
            VerdictAction::Allow
        }
    }
}

/// Size limits applied before any model work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputLimits {
    pub max_words: usize,
    pub max_bytes: usize,
}

#[domain_model]
pub struct Service {
    backend: Arc<dyn PiiBackend>,
    category_prefix: String,
    limits: InputLimits,
    thresholds: Thresholds,
}

impl Service {
    #[must_use]
    pub fn new(
        backend: Arc<dyn PiiBackend>,
        category_prefix: String,
        limits: InputLimits,
        thresholds: Thresholds,
    ) -> Self {
        Self {
            backend,
            category_prefix,
            limits,
            thresholds,
        }
    }

    /// Size checks, cheapest first. The byte cap also bounds inputs with no
    /// word breaks at all, which the word count would see as one word.
    fn check_size(&self, payload: &str) -> Result<(), DomainError> {
        if payload.len() > self.limits.max_bytes {
            return Err(DomainError::InputTooLarge {
                bytes: payload.len(),
                limit: self.limits.max_bytes,
            });
        }
        // Stops counting one past the limit: the exact figure is not needed.
        let words = payload
            .split_whitespace()
            .take(self.limits.max_words.saturating_add(1))
            .count();
        if words > self.limits.max_words {
            return Err(DomainError::InputTooManyWords {
                limit: self.limits.max_words,
            });
        }
        Ok(())
    }
}

fn severity_for_action(action: VerdictAction) -> Severity {
    match action {
        VerdictAction::Block => Severity::Critical,
        VerdictAction::Redact => Severity::High,
        VerdictAction::Warn => Severity::Medium,
        VerdictAction::Allow | VerdictAction::Audit => Severity::Low,
    }
}

/// Maps every detected entity to a `Detection` — a message can contain more
/// than one distinct piece of PII (an email *and* a phone number, say), and
/// reporting only the single highest-confidence one would silently miss
/// the rest. Entities that don't even clear the warn threshold are dropped
/// rather than reported as noise-level `Allow` findings.
fn detections_from_entities(
    entities: Vec<Entity>,
    category_prefix: &str,
    thresholds: Thresholds,
) -> Vec<Detection> {
    entities
        .into_iter()
        .filter_map(|entity| {
            let action = thresholds.action_for(entity.confidence);
            if action == VerdictAction::Allow {
                return None;
            }
            // prompt-scanner stamps the backend's identity and `version()`.
            Some(
                Detection::new(
                    format!("{category_prefix}.{}", entity.category.to_lowercase()),
                    severity_for_action(action),
                    entity.confidence,
                    action,
                )
                .with_span(Span {
                    start: entity.start,
                    end: entity.end,
                }),
            )
        })
        .collect()
}

#[async_trait]
impl ScannerBackendClient for Service {
    async fn detect(
        &self,
        _ctx: &SecurityContext,
        input: &ScanInput,
    ) -> Result<BackendOutcome, PromptScannerError> {
        self.check_size(&input.payload)
            .map_err(DomainError::into_scanner_error)?;

        let entities = self
            .backend
            .detect_entities(&input.payload)
            .await
            .map_err(DomainError::into_scanner_error)?;
        Ok(BackendOutcome::Detections(detections_from_entities(
            entities,
            &self.category_prefix,
            self.thresholds,
        )))
    }

    fn version(&self) -> Option<String> {
        Some(env!("CARGO_PKG_VERSION").to_owned())
    }
}

#[cfg(test)]
mod tests {
    use prompt_scanner_sdk::ContentKind;
    use uuid::Uuid;

    use super::*;
    use crate::runtime::RuntimeError;

    fn ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(Uuid::new_v4())
            .build()
            .expect("valid SecurityContext")
    }

    fn input(payload: &str) -> ScanInput {
        ScanInput::new(Uuid::nil(), ContentKind::Prompt, payload)
    }

    fn thresholds() -> Thresholds {
        Thresholds::new(0.6, 0.3).expect("valid")
    }

    fn entity(category: &str, start: u32, end: u32, confidence: f32) -> Entity {
        Entity {
            category: category.to_owned(),
            start,
            end,
            confidence,
        }
    }

    /// Fake backend: returns a fixed result regardless of input, so
    /// `Service`'s orchestration (size limits, threshold-to-action mapping,
    /// `Detection` construction, error mapping) can be tested without a real
    /// model.
    enum FakeBackend {
        Entities(Vec<Entity>),
        Fails(fn() -> DomainError),
    }

    #[async_trait]
    impl PiiBackend for FakeBackend {
        async fn detect_entities(&self, _text: &str) -> Result<Vec<Entity>, DomainError> {
            match self {
                Self::Entities(e) => Ok(e.clone()),
                Self::Fails(make) => Err(make()),
            }
        }
    }

    fn service_with(backend: FakeBackend, max_words: usize, max_bytes: usize) -> Service {
        Service::new(
            Arc::new(backend),
            "pii".to_owned(),
            InputLimits {
                max_words,
                max_bytes,
            },
            thresholds(),
        )
    }

    fn service(entities: Vec<Entity>, max_words: usize) -> Service {
        service_with(FakeBackend::Entities(entities), max_words, 1 << 20)
    }

    #[test]
    fn action_picks_redact_at_or_above_threshold() {
        assert_eq!(thresholds().action_for(0.6), VerdictAction::Redact);
        assert_eq!(thresholds().action_for(0.9), VerdictAction::Redact);
    }

    #[test]
    fn action_picks_warn_between_thresholds() {
        assert_eq!(thresholds().action_for(0.3), VerdictAction::Warn);
        assert_eq!(thresholds().action_for(0.59), VerdictAction::Warn);
    }

    #[test]
    fn action_picks_allow_below_warn_threshold() {
        assert_eq!(thresholds().action_for(0.0), VerdictAction::Allow);
        assert_eq!(thresholds().action_for(0.29), VerdictAction::Allow);
    }

    #[test]
    fn thresholds_reject_out_of_range_and_inverted_pairs() {
        assert!(Thresholds::new(f32::NAN, 0.3).is_err());
        assert!(Thresholds::new(0.6, f32::NAN).is_err());
        assert!(Thresholds::new(1.1, 0.3).is_err());
        assert!(Thresholds::new(0.6, -0.1).is_err());
        assert!(Thresholds::new(0.3, 0.6).is_err(), "warn above redact");
        assert!(Thresholds::new(0.5, 0.5).is_ok());
    }

    #[test]
    fn severity_matches_action() {
        assert_eq!(
            severity_for_action(VerdictAction::Block),
            Severity::Critical
        );
        assert_eq!(severity_for_action(VerdictAction::Redact), Severity::High);
        assert_eq!(severity_for_action(VerdictAction::Warn), Severity::Medium);
        assert_eq!(severity_for_action(VerdictAction::Audit), Severity::Low);
        assert_eq!(severity_for_action(VerdictAction::Allow), Severity::Low);
    }

    #[test]
    fn detections_from_entities_of_empty_is_empty() {
        assert!(detections_from_entities(Vec::new(), "pii", thresholds()).is_empty());
    }

    #[test]
    fn detections_from_entities_drops_below_warn_threshold() {
        let e = entity("EMAIL", 0, 5, 0.1);
        assert!(detections_from_entities(vec![e], "pii", thresholds()).is_empty());
    }

    #[test]
    fn every_entity_above_warn_becomes_its_own_detection() {
        let detections = detections_from_entities(
            vec![
                entity("EMAIL", 0, 10, 0.95),
                entity("Phone", 20, 32, 0.4),
                entity("PERSON", 40, 45, 0.1),
            ],
            "pii",
            thresholds(),
        );
        assert_eq!(detections.len(), 2, "the low-confidence one is dropped");

        assert_eq!(detections[0].category, "pii.email");
        assert_eq!(detections[0].suggested_action, VerdictAction::Redact);
        assert_eq!(detections[0].severity, Severity::High);
        assert_eq!(detections[0].span, Some(Span { start: 0, end: 10 }));

        assert_eq!(detections[1].category, "pii.phone", "lowercased");
        assert_eq!(detections[1].suggested_action, VerdictAction::Warn);
        assert_eq!(detections[1].severity, Severity::Medium);
        assert_eq!(detections[1].span, Some(Span { start: 20, end: 32 }));
        assert!((detections[1].confidence - 0.4).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn detect_runs_full_pipeline_and_redacts_high_confidence_pii() {
        let outcome = service(vec![entity("EMAIL", 8, 24, 0.95)], 100)
            .detect(&ctx(), &input("contact jane at example.com now"))
            .await
            .expect("scans");

        let BackendOutcome::Detections(findings) = outcome else {
            panic!("expected a Detections outcome");
        };
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].category, "pii.email");
        assert_eq!(findings[0].suggested_action, VerdictAction::Redact);
    }

    #[tokio::test]
    async fn word_limit_is_inclusive_and_over_it_is_an_error_not_a_defer() {
        let five = "contact jane at example.com now";
        let at_limit = service(Vec::new(), 5).detect(&ctx(), &input(five)).await;
        assert!(
            matches!(at_limit, Ok(BackendOutcome::Detections(_))),
            "exactly max_words is scanned: {at_limit:?}"
        );

        let over = service(Vec::new(), 4).detect(&ctx(), &input(five)).await;
        assert!(
            matches!(over, Err(PromptScannerError::Validation { .. })),
            "one word over is refused, never deferred: {over:?}"
        );
    }

    #[tokio::test]
    async fn byte_limit_catches_inputs_without_word_breaks() {
        let blob = "x".repeat(65);
        let out = service_with(FakeBackend::Entities(Vec::new()), 100, 64)
            .detect(&ctx(), &input(&blob))
            .await;
        assert!(
            matches!(out, Err(PromptScannerError::Validation { .. })),
            "{out:?}"
        );
        let ok = service_with(FakeBackend::Entities(Vec::new()), 100, 65)
            .detect(&ctx(), &input(&blob))
            .await;
        assert!(ok.is_ok(), "exactly max_bytes is scanned");
    }

    #[tokio::test]
    async fn backend_failure_is_an_error_not_a_clean_scan() {
        let svc = service_with(
            FakeBackend::Fails(|| {
                DomainError::runtime(
                    "m",
                    RuntimeError::Timeout {
                        model_id: "m".to_owned(),
                        elapsed_ms: 10,
                    },
                )
            }),
            100,
            1 << 20,
        );
        let out = svc.detect(&ctx(), &input("hello")).await;
        assert!(
            matches!(out, Err(PromptScannerError::BackendTimeout { .. })),
            "{out:?}"
        );
    }
}
