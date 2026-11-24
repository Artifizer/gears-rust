pub mod backend;
pub mod bio_backend;
mod bio_token_classifier;
pub mod entity;
pub mod error;
pub mod gliner2;
mod service;

use std::sync::Arc;

pub use service::{InputLimits, Service, Thresholds};

use crate::domain::backend::PiiBackend;
use crate::domain::bio_backend::BioTokenClassifierBackend;
use crate::domain::error::DomainError;
use crate::domain::gliner2::Gliner2Backend;
use crate::runtime::ModelRuntime;

/// Which backend to build, with everything it needs. The gear derives this
/// from its config, the evaluation tool from its command line; both go
/// through [`build_backend`] so they always run the same construction.
#[derive(Debug, Clone)]
pub enum BackendSpec {
    Bio {
        model_id: String,
        labels: Vec<String>,
    },
    Gliner2 {
        model_id: String,
        labels: Vec<String>,
        decode_threshold: f32,
        /// The model's `max_text_bytes`, enforced by the backend itself.
        max_text_bytes: usize,
    },
}

/// Builds and checks the backend `spec` describes.
///
/// # Errors
///
/// [`DomainError`] when the backend cannot run against this model host: an
/// unknown model, labels the model contradicts, a foreign tokenizer, or a
/// host that cannot be asked.
pub async fn build_backend(
    runtime: Arc<dyn ModelRuntime>,
    spec: BackendSpec,
) -> Result<Arc<dyn PiiBackend>, DomainError> {
    match spec {
        BackendSpec::Bio { model_id, labels } => {
            let backend = BioTokenClassifierBackend::new(runtime, model_id, labels);
            if !backend.verify().await? {
                // `require_all_models` already decided whether this stops
                // startup; every scan reports the model as unavailable.
                tracing::warn!(
                    "onnx-scanner: the BIO model is configured but not loaded; \
                     scans will fail as unavailable until it is"
                );
            }
            Ok(Arc::new(backend))
        }
        BackendSpec::Gliner2 {
            model_id,
            labels,
            decode_threshold,
            max_text_bytes,
        } => {
            let backend =
                Gliner2Backend::load(runtime, model_id, labels, decode_threshold, max_text_bytes)
                    .await?;
            tracing::info!(
                prefix_tokens = backend.prefix_tokens(),
                "onnx-scanner: gliner2 schema prefix built"
            );
            Ok(Arc::new(backend))
        }
    }
}
