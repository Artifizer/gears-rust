//! [`PiiBackend`] implementation for a plain BIO token-classification model
//! (`minilm-l6-pii` and similar). Calls `ModelRuntime` for tokenization and inference;
//! BIO-specific decoding lives in `domain::bio_token_classifier`.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_macros::domain_model;

use crate::domain::backend::PiiBackend;
use crate::domain::bio_token_classifier::{argmax_predictions, merge_bio};
use crate::domain::entity::Entity;
use crate::domain::error::DomainError;
use crate::runtime::{
    LoadedModelInfo, ModelRuntime, TensorData, TokenEncoding, token_classifier_inputs,
};

/// What `list_models` says about one model.
pub enum ModelLookup {
    /// Not configured at all — a typo in `model_id`.
    Absent,
    /// Configured, but not serving (failed to load, or still loading).
    NotLoaded,
    Loaded(LoadedModelInfo),
}

/// Looks `model_id` up in the model host's inventory.
pub async fn lookup_model(
    runtime: &dyn ModelRuntime,
    model_id: &str,
) -> Result<ModelLookup, DomainError> {
    let models = runtime
        .list_models()
        .await
        .map_err(|e| DomainError::runtime(model_id, e))?;
    Ok(match models.into_iter().find(|m| m.model_id == model_id) {
        None => ModelLookup::Absent,
        Some(m) => m
            .into_loaded()
            .map_or(ModelLookup::NotLoaded, ModelLookup::Loaded),
    })
}

#[domain_model]
pub struct BioTokenClassifierBackend {
    /// Resolved on first use; see [`Self::declared_inputs`].
    input_names: tokio::sync::OnceCell<Vec<String>>,
    onnx_client: Arc<dyn ModelRuntime>,
    model_id: String,
    labels: Vec<String>,
}

impl BioTokenClassifierBackend {
    #[must_use]
    pub fn new(onnx_client: Arc<dyn ModelRuntime>, model_id: String, labels: Vec<String>) -> Self {
        Self {
            input_names: tokio::sync::OnceCell::new(),
            onnx_client,
            model_id,
            labels,
        }
    }

    /// Init-time check of the configured model against the model host.
    ///
    /// A `model_id` the host does not know is an error: every scan would fail
    /// while the health check stayed green. A configured model that failed to
    /// load is reported as `Ok(false)` — the host's readiness already shows
    /// it, and `require_all_models` decides whether that stops startup. A
    /// loaded model that publishes an `id2label` map must agree with the
    /// configured labels, or every logit would be read against the wrong
    /// label.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidConfig`] for an unknown model or a label list the
    /// model contradicts; a runtime error if the host cannot be asked.
    pub async fn verify(&self) -> Result<bool, DomainError> {
        match lookup_model(self.onnx_client.as_ref(), &self.model_id).await? {
            ModelLookup::Absent => Err(DomainError::InvalidConfig(format!(
                "model `{}` is not configured in the model host",
                self.model_id
            ))),
            ModelLookup::NotLoaded => Ok(false),
            ModelLookup::Loaded(info) => {
                if let Some(published) = &info.labels
                    && *published != self.labels
                {
                    return Err(DomainError::InvalidConfig(format!(
                        "model `{}` publishes {} labels that differ from the {} configured; \
                         `backend.labels` must list the model's labels in logit order",
                        self.model_id,
                        published.len(),
                        self.labels.len()
                    )));
                }
                // Caching here saves the first scan a `list_models` round
                // trip. Already set means a scan cached the same names first.
                if self.input_names.set(info.input_names).is_err() {
                    tracing::debug!(model_id = %self.model_id, "input names already cached");
                }
                Ok(true)
            }
        }
    }

    /// The graph's declared input names, resolved once and cached.
    ///
    /// Read from `list_models` rather than configured, so it cannot drift from
    /// the graph. Cached because it is fixed for the life of a loaded model and
    /// this is on the request path.
    async fn declared_inputs(&self) -> Result<&[String], DomainError> {
        let names = self
            .input_names
            .get_or_try_init(|| async {
                match lookup_model(self.onnx_client.as_ref(), &self.model_id).await? {
                    ModelLookup::Loaded(info) => Ok(info.input_names),
                    ModelLookup::Absent | ModelLookup::NotLoaded => {
                        Err(DomainError::ModelNotLoaded {
                            model_id: self.model_id.clone(),
                        })
                    }
                }
            })
            .await?;
        Ok(names)
    }
}

#[async_trait]
impl PiiBackend for BioTokenClassifierBackend {
    async fn detect_entities(&self, text: &str) -> Result<Vec<Entity>, DomainError> {
        let model_id = self.model_id.as_str();
        let encoding = self
            .onnx_client
            .encode(model_id, text)
            .await
            .map_err(|e| DomainError::runtime(model_id, e))?;

        // Built from what this graph declares, not from a fixed list.
        // `token_type_ids` is declared by some BERT token-classification
        // exports and not others — `minilm-l6-pii` has it, `piiranha-v1` and
        // `multilang-pii-ner` do not — and both omitting a declared input and
        // supplying an undeclared one are hard errors. Hardcoding the set made
        // this backend silently specific to one model.
        let TokenEncoding {
            ids,
            attention_mask,
            offsets,
            special_tokens_mask,
        } = encoding;
        let seq_len = ids.len();
        let declared = self.declared_inputs().await?;
        let inputs = token_classifier_inputs(model_id, ids, attention_mask, declared)
            .map_err(|e| DomainError::runtime(model_id, e))?;

        let outputs = self
            .onnx_client
            .run(model_id, inputs)
            .await
            .map_err(|e| DomainError::runtime(model_id, e))?;

        let logits = outputs
            .iter()
            .find(|t| t.name == "logits")
            .ok_or_else(|| DomainError::contract(model_id, "model returned no 'logits' output"))?;
        let TensorData::F32(values) = &logits.data else {
            return Err(DomainError::contract(
                model_id,
                "'logits' output was not F32",
            ));
        };
        // `[batch = 1, seq_len, labels]` (or without the batch axis). The
        // label axis must be the configured label count, and the data must
        // be exactly that many values per token.
        if logits.shape.last() != Some(&self.labels.len()) {
            return Err(DomainError::contract(
                model_id,
                format!(
                    "'logits' shape {:?} does not end in the {} configured labels",
                    logits.shape,
                    self.labels.len()
                ),
            ));
        }

        let predictions = argmax_predictions(values, seq_len, &self.labels)
            .map_err(|e| DomainError::contract(model_id, e.to_string()))?;
        Ok(merge_bio(&predictions, &offsets, &special_tokens_mask))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{
        ModelFailureReason, ModelInfo, ModelIntegrity, NamedTensor, RuntimeError,
    };

    const MODEL: &str = "minilm-l6-pii";

    fn labels() -> Vec<String> {
        ["O", "B-EMAIL", "I-EMAIL"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    }

    /// Fake model host: `encode` returns one token per whitespace-separated
    /// word (byte offsets computed from the real payload), `run` returns
    /// logits favoring `B-EMAIL` for the first token and `O` for the rest —
    /// enough to exercise the full tokenize -> infer -> decode -> Entity
    /// pipeline without a real model. The knobs break one thing at a time.
    struct FakeOnnxRunner {
        /// Label axis of the returned logits.
        logit_labels: usize,
        /// Drops the last logit, so the data disagrees with the shape.
        truncate_logits: bool,
        /// What `list_models` reports for `MODEL`.
        model: Option<ModelInfo>,
        fail_encode: bool,
    }

    impl Default for FakeOnnxRunner {
        fn default() -> Self {
            Self {
                logit_labels: 3,
                truncate_logits: false,
                model: Some(loaded(None)),
                fail_encode: false,
            }
        }
    }

    /// Declares the two inputs a `token_type_ids`-free export has, so the
    /// backend builds exactly those. A model that declared three would get
    /// three; that variability is the point of asking.
    fn loaded(published_labels: Option<Vec<String>>) -> ModelInfo {
        ModelInfo::ready(
            MODEL,
            LoadedModelInfo {
                input_names: vec!["input_ids".to_owned(), "attention_mask".to_owned()],
                output_names: vec!["logits".to_owned()],
                integrity: ModelIntegrity::Unverified,
                replicas: 1,
                labels: published_labels,
            },
        )
    }

    fn unloaded() -> ModelInfo {
        ModelInfo::unavailable(MODEL, ModelFailureReason::SessionBuildFailed)
    }

    #[async_trait]
    impl ModelRuntime for FakeOnnxRunner {
        async fn run(
            &self,
            _model_id: &str,
            inputs: Vec<NamedTensor>,
        ) -> Result<Vec<NamedTensor>, RuntimeError> {
            let seq_len = inputs[0].shape[1];
            let n = self.logit_labels;
            // Every token strongly favors "O" (index 0)...
            let mut logits = vec![0.1f32; seq_len * n];
            for i in 0..seq_len {
                logits[i * n] = 5.0;
            }
            // ...except the first, which strongly favors B-EMAIL (index 1).
            logits[0] = 0.1;
            logits[1] = 5.0;
            if self.truncate_logits {
                logits.pop();
            }
            Ok(vec![NamedTensor::new(
                "logits",
                vec![1, seq_len, n],
                TensorData::F32(logits),
            )])
        }

        async fn encode(&self, model_id: &str, text: &str) -> Result<TokenEncoding, RuntimeError> {
            if self.fail_encode {
                return Err(RuntimeError::Overloaded {
                    model_id: model_id.to_owned(),
                    max_in_flight: 1,
                });
            }
            let mut ids = Vec::new();
            let mut offsets = Vec::new();
            let mut cursor = 0usize;
            for word in text.split_whitespace() {
                let start = text[cursor..].find(word).map_or(cursor, |i| cursor + i);
                let end = start + word.len();
                ids.push(1);
                offsets.push((
                    u32::try_from(start).expect("short fixture"),
                    u32::try_from(end).expect("short fixture"),
                ));
                cursor = end;
            }
            let len = ids.len();
            Ok(TokenEncoding {
                ids,
                attention_mask: vec![1; len],
                offsets,
                special_tokens_mask: vec![false; len],
            })
        }

        async fn encode_fragment(
            &self,
            _model_id: &str,
            _word: &str,
        ) -> Result<Vec<i64>, RuntimeError> {
            unimplemented!("not used by the BIO token classifier backend")
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, RuntimeError> {
            Ok(self.model.clone().into_iter().collect())
        }
    }

    fn backend(fake: FakeOnnxRunner) -> BioTokenClassifierBackend {
        BioTokenClassifierBackend::new(Arc::new(fake), MODEL.to_owned(), labels())
    }

    #[tokio::test]
    async fn detects_entity_from_first_token() {
        let text = "contact jane at example.com now";
        let entities = backend(FakeOnnxRunner::default())
            .detect_entities(text)
            .await
            .expect("detects");

        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].category, "EMAIL");
        assert_eq!((entities[0].start, entities[0].end), (0, 7));
        assert_eq!(&text[0..7], "contact");
        assert!(entities[0].confidence > 0.9);
    }

    #[tokio::test]
    async fn logits_with_a_different_label_count_are_a_contract_error() {
        // A 4-label model decoded against 3 configured labels would read
        // every row at the wrong stride; it must fail, not return a clean scan.
        let fake = FakeOnnxRunner {
            logit_labels: 4,
            ..FakeOnnxRunner::default()
        };
        assert!(matches!(
            backend(fake).detect_entities("a b c").await,
            Err(DomainError::OutputContract { .. })
        ));
    }

    #[tokio::test]
    async fn logits_shorter_than_their_shape_are_a_contract_error() {
        let fake = FakeOnnxRunner {
            truncate_logits: true,
            ..FakeOnnxRunner::default()
        };
        assert!(matches!(
            backend(fake).detect_entities("a b c").await,
            Err(DomainError::OutputContract { .. })
        ));
    }

    #[tokio::test]
    async fn runtime_errors_keep_their_classification() {
        let fake = FakeOnnxRunner {
            fail_encode: true,
            ..FakeOnnxRunner::default()
        };
        assert!(matches!(
            backend(fake).detect_entities("a").await,
            Err(DomainError::Runtime {
                source: RuntimeError::Overloaded { .. },
                ..
            })
        ));
    }

    #[tokio::test]
    async fn an_unloaded_model_is_unavailable_not_clean() {
        let fake = FakeOnnxRunner {
            model: Some(unloaded()),
            ..FakeOnnxRunner::default()
        };
        assert!(matches!(
            backend(fake).detect_entities("a").await,
            Err(DomainError::ModelNotLoaded { .. })
        ));
    }

    #[tokio::test]
    async fn verify_rejects_an_unknown_model() {
        let fake = FakeOnnxRunner {
            model: None,
            ..FakeOnnxRunner::default()
        };
        assert!(matches!(
            backend(fake).verify().await,
            Err(DomainError::InvalidConfig(_))
        ));
    }

    #[tokio::test]
    async fn verify_reports_a_configured_but_unloaded_model() {
        let fake = FakeOnnxRunner {
            model: Some(unloaded()),
            ..FakeOnnxRunner::default()
        };
        assert!(!backend(fake).verify().await.expect("not an error"));
    }

    #[tokio::test]
    async fn verify_rejects_labels_the_model_contradicts() {
        let fake = FakeOnnxRunner {
            model: Some(loaded(Some(vec!["O".to_owned(), "B-PER".to_owned()]))),
            ..FakeOnnxRunner::default()
        };
        assert!(matches!(
            backend(fake).verify().await,
            Err(DomainError::InvalidConfig(_))
        ));

        let agreeing = FakeOnnxRunner {
            model: Some(loaded(Some(labels()))),
            ..FakeOnnxRunner::default()
        };
        assert!(backend(agreeing).verify().await.expect("labels agree"));
    }
}
