//! Fastino `GLiNER2` PII detector — a zero-shot span model specialized for
//! PII, replacing the BIO token classifier as `onnx-scanner`'s default
//! backend for the `GenAI` Security demo (`config/genai-security.yaml`).
//!
//! Ported from the `dlp-bench` detector-benchmark repo's
//! `src/detectors/gliner2/` (a from-scratch, tested Rust port of the
//! reference `GLiNER2` `PyTorch` inference path), adapted to run its ONNX graph
//! through the model host's generic `ModelRuntime::run`/`encode_fragment`
//! instead of holding a private `ort` session — the plugin's model host
//! stays the only owner of ONNX Runtime resources.
//!
//! # Provenance and its caveats
//!
//! The export (`yethdev/gliner2-pii-onnx`) is community-produced and
//! unaffiliated with Fastino. Its card claims span-set parity with the
//! `PyTorch` engine at thresholds 0.3/0.5/0.7, but `dlp-bench`'s own
//! measurements are the only ones this integration relies on — see that
//! repo's `REPORT.md`.
//!
//! # Structure
//!
//! * [`tensor`] — adapts `crate::runtime::NamedTensor` to the plain
//!   `(shape, data)` view `decode` needs.
//! * [`words`] — text preparation and the reference word splitter (ported
//!   verbatim, self-contained, no `ort`/tokenizer dependency).
//! * [`prompt`] — schema prefix and per-document encoder input, built via
//!   `ModelRuntime::encode_fragment`.
//! * [`decode`] — span extraction and per-label suppression.

pub mod decode;
pub mod prompt;
pub mod tensor;
pub mod words;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use toolkit_macros::domain_model;

use self::prompt::{EncodedInput, PromptPrefix, WordIdCache};
use crate::domain::backend::PiiBackend;
use crate::domain::entity::Entity;
use crate::domain::error::DomainError;
use crate::runtime::{ModelRuntime, NamedTensor, TensorData};

const EXPECTED_OUTPUTS: [&str; 2] = ["span_probs", "count_logits"];

/// The model's span-width axis. Asserted against every real output.
const EXPECTED_MAX_WIDTH: usize = 8;

/// Distinct words whose sub-token ids are kept between scans.
const WORD_CACHE_CAP: usize = 50_000;

#[domain_model]
pub struct Gliner2Backend {
    onnx_client: Arc<dyn ModelRuntime>,
    model_id: String,
    labels: Vec<String>,
    /// Spans scoring below this are never emitted, independent of
    /// `Service`'s separate redact/warn action thresholds (which operate
    /// on whatever confidence a backend does emit) — see the model card's
    /// 0.3/0.5/0.7 operating points.
    decode_threshold: f32,
    /// The model's `max_text_bytes`. Enforced here because this backend
    /// tokenizes word by word through `encode_fragment`, which the model
    /// host's own per-text bound never sees.
    max_text_bytes: usize,
    prefix: PromptPrefix,
    /// A plain `std` mutex: it is never held across an `.await` (see
    /// `prompt`'s module docs), so scans do not serialize on it.
    cache: Mutex<WordIdCache>,
}

impl Gliner2Backend {
    /// Precomputes the schema prefix (via `onnx_client.encode_fragment`, so
    /// this is async and must run once at gear `init()`, not per scan —
    /// see `PromptPrefix`'s docs on why it's safe to reuse across
    /// documents).
    ///
    /// # Errors
    ///
    /// Returns [`DomainError`] if the model host is unavailable, the
    /// configured `model_id` isn't loaded, the label set is empty, or the
    /// tokenizer's special-token ids don't match what this backend expects
    /// (wrong tokenizer for this model).
    pub async fn load(
        onnx_client: Arc<dyn ModelRuntime>,
        model_id: String,
        labels: Vec<String>,
        decode_threshold: f32,
        max_text_bytes: usize,
    ) -> Result<Self, DomainError> {
        prompt::assert_special_tokens(onnx_client.as_ref(), &model_id).await?;
        let prefix = prompt::build_prefix(onnx_client.as_ref(), &model_id, &labels).await?;

        Ok(Self {
            onnx_client,
            model_id,
            labels,
            decode_threshold,
            max_text_bytes,
            prefix,
            cache: Mutex::new(WordIdCache::new(WORD_CACHE_CAP)),
        })
    }

    /// Prefix length in tokens — the fixed per-call cost imposed by the
    /// schema (dominant term in short-prompt latency; see the module docs
    /// this was ported from).
    pub fn prefix_tokens(&self) -> usize {
        self.prefix.len()
    }

    fn contract(&self, detail: impl Into<String>) -> DomainError {
        DomainError::contract(&self.model_id, detail)
    }

    fn offset(&self, v: usize) -> Result<u32, DomainError> {
        u32::try_from(v).map_err(|_| self.contract("span offset does not fit in u32"))
    }
}

#[async_trait]
impl PiiBackend for Gliner2Backend {
    async fn detect_entities(&self, text: &str) -> Result<Vec<Entity>, DomainError> {
        if text.len() > self.max_text_bytes {
            return Err(DomainError::InputTooLarge {
                bytes: text.len(),
                limit: self.max_text_bytes,
            });
        }
        let prepared = words::prepare(text);
        if prepared.words.is_empty() {
            return Ok(Vec::new());
        }

        let enc: EncodedInput = prompt::build_input(
            &self.prefix,
            &prepared.words,
            self.onnx_client.as_ref(),
            &self.model_id,
            &self.cache,
        )
        .await?;

        let seq_len = enc.seq_len();
        let word_count = enc.text_word_indices.len();
        let inputs = vec![
            NamedTensor::new(
                "input_ids",
                vec![1, seq_len],
                TensorData::I64(enc.input_ids),
            ),
            NamedTensor::new(
                "attention_mask",
                vec![1, seq_len],
                TensorData::I64(enc.attention_mask),
            ),
            NamedTensor::new(
                "text_word_indices",
                vec![word_count],
                TensorData::I64(enc.text_word_indices),
            ),
            NamedTensor::new(
                "schema_special_indices",
                vec![self.prefix.schema_special_indices.len()],
                TensorData::I64(self.prefix.schema_special_indices.clone()),
            ),
        ];

        let outputs = self
            .onnx_client
            .run(&self.model_id, inputs)
            .await
            .map_err(|e| DomainError::runtime(&self.model_id, e))?;

        let [span_name, count_name] = EXPECTED_OUTPUTS;
        let span_probs = tensor::f32_tensor(
            &self.model_id,
            tensor::find_tensor(&self.model_id, &outputs, span_name)?,
        )?;
        let count_logits = tensor::f32_tensor(
            &self.model_id,
            tensor::find_tensor(&self.model_id, &outputs, count_name)?,
        )?;

        let dims =
            decode::SpanDims::from_tensor(span_probs).map_err(|e| self.contract(e.to_string()))?;
        if dims.max_width != EXPECTED_MAX_WIDTH {
            return Err(self.contract(format!(
                "expected span width axis {EXPECTED_MAX_WIDTH}, got {} (shape {:?})",
                dims.max_width, span_probs.shape
            )));
        }
        if dims.num_labels != self.prefix.num_labels {
            return Err(self.contract(format!(
                "graph returned {} labels but {} were requested",
                dims.num_labels, self.prefix.num_labels
            )));
        }

        let raw = decode::decode(
            span_probs,
            dims,
            count_logits,
            &prepared.words,
            &prepared.text,
            self.decode_threshold,
        )
        .map_err(|e| self.contract(e.to_string()))?;

        // Offsets are into the prepared text, which may carry a synthetic
        // trailing '.'. Clip to the caller's text so a returned span is
        // always sliceable against what they passed in.
        let limit = prepared.original_len();
        let mut out = Vec::with_capacity(raw.len());
        for s in raw {
            let Some(category) = self.labels.get(s.label_idx) else {
                return Err(self.contract(format!(
                    "decoded label index {} is outside the {}-label schema",
                    s.label_idx,
                    self.labels.len()
                )));
            };
            let end = s.end.min(limit);
            if s.start >= end {
                continue; // the span was entirely the synthetic '.'
            }
            out.push(Entity {
                category: category.clone(),
                start: self.offset(s.start)?,
                end: self.offset(end)?,
                confidence: s.score,
            });
        }

        Ok(out)
    }
}

/// A scriptable [`ModelRuntime`] for the `GLiNER2` tests.
#[cfg(test)]
pub mod test_support {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::prompt::special;
    use crate::runtime::{
        ModelInfo, ModelRuntime, NamedTensor, RuntimeError, TensorData, TokenEncoding,
    };

    /// Every ordinary word is one sub-token (a stable fake id), except
    /// `"longword"` (two) and [`Self::EMPTY_WORD`] (zero); the special tokens
    /// map to their real ids. `run` answers with `span_probs` scored by
    /// `cells` and an open count gate.
    #[derive(Default)]
    pub struct FakeRuntime {
        pub wrong_special_tokens: bool,
        /// `(start_word, width, label, prob)` cells set in `span_probs`.
        pub cells: Vec<(usize, usize, usize, f32)>,
        /// Overrides the label axis `run` reports (defaults to the schema's).
        pub labels_axis: Option<usize>,
        pub count_gate_closed: bool,
        pub fail_run: Option<fn() -> RuntimeError>,
        pub fragment_calls: AtomicUsize,
        pub last_inputs: Mutex<Vec<NamedTensor>>,
    }

    impl FakeRuntime {
        pub const EMPTY_WORD: &'static str = "zzempty";

        pub fn fragment_calls(&self) -> usize {
            self.fragment_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ModelRuntime for FakeRuntime {
        async fn run(
            &self,
            _model_id: &str,
            inputs: Vec<NamedTensor>,
        ) -> Result<Vec<NamedTensor>, RuntimeError> {
            if let Some(fail) = self.fail_run {
                return Err(fail());
            }
            let words = inputs
                .iter()
                .find(|t| t.name == "text_word_indices")
                .map_or(0, |t| t.shape[0]);
            let schema = inputs
                .iter()
                .find(|t| t.name == "schema_special_indices")
                .map_or(1, |t| t.shape[0]);
            let labels = self.labels_axis.unwrap_or(schema - 1);
            let mut data = vec![0.0f32; words * 8 * labels];
            for &(s, w, l, p) in &self.cells {
                data[(s * 8 + w) * labels + l] = p;
            }
            let mut gate = vec![0.0f32; 20];
            gate[usize::from(!self.count_gate_closed)] = 1.0;
            *self.last_inputs.lock().expect("not poisoned") = inputs;
            Ok(vec![
                NamedTensor::new("span_probs", vec![words, 8, labels], TensorData::F32(data)),
                NamedTensor::new("count_logits", vec![1, 20], TensorData::F32(gate)),
            ])
        }

        async fn encode(
            &self,
            _model_id: &str,
            _text: &str,
        ) -> Result<TokenEncoding, RuntimeError> {
            unimplemented!("gliner2 never encodes whole documents")
        }

        async fn encode_fragment(
            &self,
            _model_id: &str,
            fragment: &str,
        ) -> Result<Vec<i64>, RuntimeError> {
            self.fragment_calls.fetch_add(1, Ordering::SeqCst);
            let shift = i64::from(self.wrong_special_tokens);
            Ok(match fragment {
                special::P => vec![special::P_ID + shift],
                special::E => vec![special::E_ID + shift],
                special::SEP_TEXT => vec![special::SEP_TEXT_ID + shift],
                "longword" => vec![7, 8],
                Self::EMPTY_WORD => Vec::new(),
                other => vec![i64::try_from(other.len()).expect("short") + 100],
            })
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, RuntimeError> {
            Ok(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeRuntime;
    use super::*;
    use crate::runtime::RuntimeError;

    const LABELS: [&str; 2] = ["person", "email"];

    async fn backend(rt: FakeRuntime, max_text_bytes: usize) -> (Gliner2Backend, Arc<FakeRuntime>) {
        let rt = Arc::new(rt);
        let backend = Gliner2Backend::load(
            Arc::clone(&rt) as Arc<dyn ModelRuntime>,
            "gliner2".to_owned(),
            LABELS.iter().map(|s| (*s).to_owned()).collect(),
            0.5,
            max_text_bytes,
        )
        .await
        .expect("loads");
        (backend, rt)
    }

    #[tokio::test]
    async fn detects_a_span_and_maps_it_back_to_caller_bytes() {
        // Words: "mail"(0) "jane@example.com"(1) "now"(2) "."(3)
        let rt = FakeRuntime {
            cells: vec![(1, 0, 1, 0.9), (0, 0, 0, 0.2)],
            ..FakeRuntime::default()
        };
        let (b, rt) = backend(rt, 1024).await;
        let text = "mail jane@example.com now";
        let entities = b.detect_entities(text).await.expect("detects");

        assert_eq!(entities.len(), 1, "the 0.2 cell is under decode_threshold");
        assert_eq!(entities[0].category, "email");
        assert_eq!(
            &text[entities[0].start as usize..entities[0].end as usize],
            "jane@example.com"
        );
        assert!((entities[0].confidence - 0.9).abs() < f32::EPSILON);

        // The graph got the four inputs it declares, consistently shaped.
        let inputs = rt.last_inputs.lock().expect("not poisoned");
        let names: Vec<&str> = inputs.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "input_ids",
                "attention_mask",
                "text_word_indices",
                "schema_special_indices"
            ]
        );
        assert_eq!(inputs[0].shape, inputs[1].shape);
        assert_eq!(inputs[2].shape, vec![4]);
        assert_eq!(inputs[3].shape, vec![LABELS.len() + 1]);
    }

    #[tokio::test]
    async fn a_span_on_the_synthetic_dot_is_clipped_away() {
        // Words: "hi"(0) "."(1, synthetic). A span covering only the dot is
        // dropped; one covering "hi ." is clipped to "hi".
        let rt = FakeRuntime {
            cells: vec![(1, 0, 0, 0.9), (0, 1, 1, 0.8)],
            ..FakeRuntime::default()
        };
        let (b, _) = backend(rt, 1024).await;
        let entities = b.detect_entities("hi").await.expect("detects");
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].category, "email");
        assert_eq!((entities[0].start, entities[0].end), (0, 2));
    }

    #[tokio::test]
    async fn a_closed_count_gate_yields_nothing() {
        let rt = FakeRuntime {
            cells: vec![(0, 0, 0, 0.99)],
            count_gate_closed: true,
            ..FakeRuntime::default()
        };
        let (b, _) = backend(rt, 1024).await;
        assert!(b.detect_entities("John").await.expect("detects").is_empty());
    }

    #[tokio::test]
    async fn a_label_axis_mismatch_is_a_contract_error() {
        let rt = FakeRuntime {
            labels_axis: Some(3),
            ..FakeRuntime::default()
        };
        let (b, _) = backend(rt, 1024).await;
        assert!(matches!(
            b.detect_entities("John").await,
            Err(DomainError::OutputContract { .. })
        ));
    }

    #[tokio::test]
    async fn runtime_failures_keep_their_classification() {
        let rt = FakeRuntime {
            fail_run: Some(|| RuntimeError::Timeout {
                model_id: "gliner2".to_owned(),
                elapsed_ms: 1,
            }),
            ..FakeRuntime::default()
        };
        let (b, _) = backend(rt, 1024).await;
        assert!(matches!(
            b.detect_entities("John").await,
            Err(DomainError::Runtime {
                source: RuntimeError::Timeout { .. },
                ..
            })
        ));
    }

    #[tokio::test]
    async fn text_over_max_text_bytes_is_refused_before_tokenizing() {
        let (b, rt) = backend(FakeRuntime::default(), 8).await;
        let before = rt.fragment_calls();
        assert!(matches!(
            b.detect_entities("nine byte").await,
            Err(DomainError::InputTooLarge { bytes: 9, limit: 8 })
        ));
        assert_eq!(rt.fragment_calls(), before, "nothing was tokenized");
        b.detect_entities("eight by")
            .await
            .expect("the limit itself is allowed");
    }

    #[tokio::test]
    async fn load_rejects_the_wrong_tokenizer() {
        let rt: Arc<dyn ModelRuntime> = Arc::new(FakeRuntime {
            wrong_special_tokens: true,
            ..FakeRuntime::default()
        });
        let loaded =
            Gliner2Backend::load(rt, "g".to_owned(), vec!["person".to_owned()], 0.5, 64).await;
        assert!(matches!(loaded, Err(DomainError::TokenizerMismatch { .. })));
    }
}
