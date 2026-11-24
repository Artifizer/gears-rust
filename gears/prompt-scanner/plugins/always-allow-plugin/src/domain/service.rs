//! Trivial "always allow" backend: inspects nothing and reports nothing.
//!
//! It is the smallest real backend plugin: the demo configuration enables it
//! next to the real detectors so `prompt-scanner`'s fan-out
//! (`domain::service::Service::resolve_backends` and `run_backends`) has a
//! no-op instance to resolve and call. It is not a detector —
//! `regex-scanner` and `onnx-scanner` are the real backends. Its own
//! behaviour is pinned by the tests below.
//!
//! # Why it reports an *empty* finding set
//!
//! A backend with nothing to say must return `Detections(vec![])`, never a
//! placeholder finding. `domain::policy::aggregate_verdict` applies the
//! tenant's `default_action` only when **no** finding survives threshold
//! filtering, so a single meaningless zero-confidence `Allow` is enough to
//! consume that fallback and silently replace a tenant's `default_action:
//! Block` with `Allow`. `default_action` is a fallback, not a floor, and
//! cannot be made one (`Block` is the maximum of the ordering, so a floor
//! would block every request). The invariant therefore lives here, and is
//! pinned by `reports_an_empty_finding_set_rather_than_a_placeholder` below.

use async_trait::async_trait;
use prompt_scanner_sdk::{BackendOutcome, PromptScannerError, ScanInput, ScannerBackendClient};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

#[domain_model]
pub struct Service;

#[async_trait]
impl ScannerBackendClient for Service {
    async fn detect(
        &self,
        _ctx: &SecurityContext,
        _input: &ScanInput,
    ) -> Result<BackendOutcome, PromptScannerError> {
        Ok(BackendOutcome::Detections(Vec::new()))
    }

    fn version(&self) -> Option<String> {
        Some(env!("CARGO_PKG_VERSION").to_owned())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use prompt_scanner_sdk::ContentKind;
    use uuid::Uuid;

    use super::*;

    fn ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_id(Uuid::new_v4())
            .subject_tenant_id(Uuid::new_v4())
            .build()
            .expect("valid SecurityContext")
    }

    fn input() -> ScanInput {
        ScanInput::new(Uuid::nil(), ContentKind::Prompt, "anything at all")
    }

    /// Reporting nothing must mean an empty finding set, not a placeholder
    /// finding — see the module docs for what a placeholder would break.
    #[tokio::test]
    async fn reports_an_empty_finding_set_rather_than_a_placeholder() {
        let outcome = Service
            .detect(&ctx(), &input())
            .await
            .expect("always-allow never errors");

        let BackendOutcome::Detections(findings) = outcome else {
            panic!("expected a Detections outcome");
        };
        assert!(
            findings.is_empty(),
            "a backend that inspects nothing must report nothing, got: {findings:?}"
        );
    }

    /// The gateway holds backends as `Arc<dyn ScannerBackendClient>`; the
    /// plugin must behave the same through that handle, for any content.
    #[tokio::test]
    async fn works_through_the_trait_object_for_every_content_kind() {
        let backend: std::sync::Arc<dyn ScannerBackendClient> = std::sync::Arc::new(Service);
        for (kind, payload) in [
            (
                ContentKind::Prompt,
                "card 4111111111111111 key AKIAABCDEFGHIJKLMNOP",
            ),
            (ContentKind::Prompt, ""),
            (ContentKind::Upload, "attachment text"),
        ] {
            let input = ScanInput::new(Uuid::new_v4(), kind, payload);
            let outcome = backend.detect(&ctx(), &input).await.expect("never errors");
            assert_eq!(outcome, BackendOutcome::Detections(Vec::new()));
        }
        assert_eq!(
            backend.version().as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }
}
