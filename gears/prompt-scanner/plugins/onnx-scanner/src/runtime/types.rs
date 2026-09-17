//! The values the model host hands its backends.
//!
//! In-process only: backends receive these by value through
//! [`super::ModelRuntime`], and nothing serializes them, so they carry no
//! wire format. An out-of-process transport would define its own DTOs.

/// A single named tensor's data, covering the dtypes real models actually
/// need — dlp-bench only ever needed I64 in / F32 out, but attention masks
/// are sometimes Bool and some tokenizer outputs are U8.
///
/// The shape is deliberately *not* here — it lives on [`NamedTensor::shape`],
/// and this array is that shape flattened in row-major order. Keeping them
/// apart means a shape/data disagreement is one validation at the boundary
/// rather than a malformed value.
#[derive(Debug, Clone, PartialEq)]
pub enum TensorData {
    I64(Vec<i64>),
    F32(Vec<f32>),
    U8(Vec<u8>),
    Bool(Vec<bool>),
}

impl TensorData {
    /// Number of elements, whatever the dtype.
    #[must_use]
    pub const fn len(&self) -> usize {
        match self {
            Self::I64(d) => d.len(),
            Self::F32(d) => d.len(),
            Self::U8(d) => d.len(),
            Self::Bool(d) => d.len(),
        }
    }

    /// Whether the tensor holds no elements.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A single named tensor, in or out.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedTensor {
    pub name: String,
    pub shape: Vec<usize>,
    pub data: TensorData,
}

impl NamedTensor {
    #[must_use]
    pub fn new(name: impl Into<String>, shape: Vec<usize>, data: TensorData) -> Self {
        Self {
            name: name.into(),
            shape,
            data,
        }
    }
}

/// Why a configured model is not usable.
///
/// A stable machine-readable classification, never a free-form diagnostic:
/// consumers branch on it, and it reaches logs, telemetry and the
/// unauthenticated `/health`, so it must never carry a path, file contents, or
/// tensor data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ModelFailureReason {
    /// The model file could not be read.
    FileUnreadable,
    /// The file's SHA-256 did not match the configured digest, before or
    /// after the sessions were opened.
    ChecksumMismatch,
    /// The loaded graph's tensor names differ from the configured contract.
    ContractMismatch,
    /// `tokenizer.json` was missing, failed its checksum, or could not be
    /// parsed.
    TokenizerInvalid,
    /// ONNX Runtime rejected the graph, or every replica was lost and could
    /// not be rebuilt.
    SessionBuildFailed,
    /// The model's configuration was rejected before any file was read.
    InvalidConfig,
}

impl ModelFailureReason {
    /// Every reason, for exhaustive tests of anything that projects them.
    pub const ALL: [Self; 6] = [
        Self::FileUnreadable,
        Self::ChecksumMismatch,
        Self::ContractMismatch,
        Self::TokenizerInvalid,
        Self::SessionBuildFailed,
        Self::InvalidConfig,
    ];

    /// A stable `snake_case` code, for logs and telemetry.
    #[must_use]
    pub const fn as_code(self) -> &'static str {
        match self {
            Self::FileUnreadable => "file_unreadable",
            Self::ChecksumMismatch => "checksum_mismatch",
            Self::ContractMismatch => "contract_mismatch",
            Self::TokenizerInvalid => "tokenizer_invalid",
            Self::SessionBuildFailed => "session_build_failed",
            Self::InvalidConfig => "invalid_config",
        }
    }
}

impl std::fmt::Display for ModelFailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_code())
    }
}

/// How a loaded model's file was checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelIntegrity {
    /// The file hashed to the configured digest before and after the sessions
    /// were opened.
    Verified { sha256: String },
    /// `skip_checksum` was set: nothing was checked, and nothing is claimed.
    Unverified,
}

/// The parts of a model's description that only exist once it has actually
/// loaded: the graph's real tensor names and how its file was verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedModelInfo {
    pub input_names: Vec<String>,
    pub output_names: Vec<String>,
    pub integrity: ModelIntegrity,
    /// Replicas currently holding a session (configured, minus any being
    /// rebuilt or lost).
    pub replicas: usize,
    /// The model's class labels, in class-id order, when it publishes an
    /// `id2label` map beside its weights.
    ///
    /// This is metadata carried in the model directory, exactly as
    /// `tokenizer.json` is — serving it is not knowledge of what the labels
    /// *mean*. Interpreting them (BIO merging, entity taxonomies, thresholds)
    /// remains the consumer's job.
    ///
    /// `None` means the model published no usable map, which is normal: a
    /// zero-shot span model takes its labels at request time and has none to
    /// declare. It is **not** a signal that the model failed to load.
    ///
    /// Indexing is positional — `labels[class_id]` — so consumers should check
    /// the length against the model's own class dimension before trusting it;
    /// the gear does not know which output carries classes.
    pub labels: Option<Vec<String>>,
}

/// Where a configured model stands.
///
/// Every *configured* model has a state, including one that never loaded —
/// that is the point. A caller that gets no entry at all is looking at a typo;
/// a caller that gets `Unavailable` is looking at an outage.
///
/// The loaded details live *inside* `Ready`, so "ready with no details" and
/// "unavailable with details" cannot be expressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelState {
    /// Loaded, verified, and serving.
    Ready(LoadedModelInfo),
    /// Configured but not usable. `reason` is stable enough to branch on.
    Unavailable { reason: ModelFailureReason },
}

/// Metadata about a configured model, for introspection (`list_models`) and
/// so a caller can fail loudly if it's pointed at the wrong model id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub model_id: String,
    pub state: ModelState,
}

impl ModelInfo {
    /// A model that loaded.
    #[must_use]
    pub fn ready(model_id: impl Into<String>, loaded: LoadedModelInfo) -> Self {
        Self {
            model_id: model_id.into(),
            state: ModelState::Ready(loaded),
        }
    }

    /// A configured model that cannot serve.
    #[must_use]
    pub fn unavailable(model_id: impl Into<String>, reason: ModelFailureReason) -> Self {
        Self {
            model_id: model_id.into(),
            state: ModelState::Unavailable { reason },
        }
    }

    /// Whether this model can serve a request right now.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self.state, ModelState::Ready(_))
    }

    /// The loaded details, when the model is ready.
    #[must_use]
    pub const fn loaded(&self) -> Option<&LoadedModelInfo> {
        match &self.state {
            ModelState::Ready(loaded) => Some(loaded),
            ModelState::Unavailable { .. } => None,
        }
    }

    /// The loaded details by value, when the model is ready.
    #[must_use]
    pub fn into_loaded(self) -> Option<LoadedModelInfo> {
        match self.state {
            ModelState::Ready(loaded) => Some(loaded),
            ModelState::Unavailable { .. } => None,
        }
    }

    /// Why the model cannot serve, when it cannot.
    #[must_use]
    pub const fn failure_reason(&self) -> Option<ModelFailureReason> {
        match self.state {
            ModelState::Ready(_) => None,
            ModelState::Unavailable { reason } => Some(reason),
        }
    }
}

/// Result of tokenizing text against a configured tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenEncoding {
    pub ids: Vec<i64>,
    pub attention_mask: Vec<i64>,
    /// Byte offsets into the original text, per token — needed to map
    /// model-space spans back to the original payload.
    pub offsets: Vec<(u32, u32)>,
    pub special_tokens_mask: Vec<bool>,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn loaded() -> LoadedModelInfo {
        LoadedModelInfo {
            input_names: vec!["input_ids".to_owned()],
            output_names: vec!["logits".to_owned()],
            integrity: ModelIntegrity::Verified {
                sha256: "b".repeat(64),
            },
            replicas: 2,
            labels: None,
        }
    }

    #[test]
    fn ready_carries_its_details_and_unavailable_carries_none() {
        let ready = ModelInfo::ready("gliner2-pii", loaded());
        assert!(ready.is_ready());
        assert_eq!(ready.loaded(), Some(&loaded()));
        assert_eq!(ready.failure_reason(), None);
        assert_eq!(ready.into_loaded(), Some(loaded()));

        let down = ModelInfo::unavailable("piiranha-v1", ModelFailureReason::FileUnreadable);
        assert!(!down.is_ready());
        assert!(down.loaded().is_none());
        assert_eq!(
            down.failure_reason(),
            Some(ModelFailureReason::FileUnreadable)
        );
        assert!(down.into_loaded().is_none());
    }

    #[test]
    fn failure_codes_are_distinct_snake_case_and_match_display() {
        let mut seen = std::collections::BTreeSet::new();
        for reason in ModelFailureReason::ALL {
            let code = reason.as_code();
            assert!(seen.insert(code), "{code} used twice");
            assert!(
                code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{code} is not snake_case"
            );
            assert_eq!(reason.to_string(), code);
        }
    }

    #[test]
    fn tensor_len_counts_elements_for_every_dtype() {
        assert_eq!(TensorData::I64(vec![1, 2, 3]).len(), 3);
        assert_eq!(TensorData::F32(vec![f32::NAN, f32::INFINITY]).len(), 2);
        assert_eq!(TensorData::U8(vec![0]).len(), 1);
        assert!(TensorData::Bool(Vec::new()).is_empty());
    }
}
