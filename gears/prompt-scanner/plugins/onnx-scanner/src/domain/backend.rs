//! The polymorphic seam between `onnx-scanner`'s orchestration
//! (`domain::service::Service` — size limits, threshold-to-action mapping,
//! `Detection` construction) and any specific PII model family.
//!
//! Two implementations exist:
//! - [`crate::domain::bio_backend::BioTokenClassifierBackend`] — a plain
//!   BIO token-classification model (`minilm-l6-pii`).
//! - [`crate::domain::gliner2::Gliner2Backend`] — Fastino's `GLiNER2`, a
//!   zero-shot span model specialized for PII.
//!
//! `config::PiiBackendConfig` selects which one `gear.rs` constructs (through
//! [`crate::domain::build_backend`]); `Service` itself never branches on
//! which — that would defeat the point of the trait. One plugin instance
//! runs exactly one config-selected backend; running two models means
//! registering two backend instances, each enabled by tenant policy.

use async_trait::async_trait;

use crate::domain::entity::Entity;
use crate::domain::error::DomainError;

/// A PII model backend: text in, detected entities out.
#[async_trait]
pub trait PiiBackend: Send + Sync {
    /// Detect PII entities in `text`. Returned spans are byte offsets into
    /// `text`.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError`] on model/runtime failure (model host
    /// unavailable, timeout, unexpected tensor shape, oversized input).
    async fn detect_entities(&self, text: &str) -> Result<Vec<Entity>, DomainError>;
}
