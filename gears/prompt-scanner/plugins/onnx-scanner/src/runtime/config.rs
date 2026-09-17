use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroUsize;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Which typed extraction to use for a given output tensor — the model's
/// own documented output dtypes are declared here rather than guessed at
/// runtime (mirrors the `GraphContract` verification philosophy: know your
/// model ahead of time, fail loudly on mismatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputDtype {
    I64,
    F32,
    U8,
    Bool,
}

/// Most session replicas one model may ask for. Each holds a resident copy of
/// the weights, so a typo here (`400` for `4`) is an out-of-memory kill.
pub const MAX_REPLICAS: usize = 64;
/// Most ONNX Runtime intra-op threads one replica may ask for.
pub const MAX_INTRA_THREADS: usize = 64;
/// Most concurrent inferences one model may admit.
pub const MAX_IN_FLIGHT: usize = 4096;
/// Longest model id accepted. Ids reach metric labels and `/health`.
pub const MAX_MODEL_ID_LEN: usize = 64;

/// A SHA-256 digest, parsed from (and printed as) 64 hex characters.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// Wraps raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Parses 64 hex characters, either case.
    ///
    /// # Errors
    ///
    /// [`DigestParseError`] when the input is not exactly 64 hex characters.
    pub fn from_hex(hex: &str) -> Result<Self, DigestParseError> {
        let raw = hex.as_bytes();
        if raw.len() != 64 {
            return Err(DigestParseError { len: raw.len() });
        }
        let mut out = [0_u8; 32];
        for (byte, &[hi, lo]) in out.iter_mut().zip(raw.as_chunks::<2>().0) {
            let (Some(hi), Some(lo)) = (hex_value(hi), hex_value(lo)) else {
                return Err(DigestParseError { len: raw.len() });
            };
            *byte = (hi << 4) | lo;
        }
        Ok(Self(out))
    }

    /// The digest as 64 lowercase hex characters.
    #[must_use]
    pub fn to_hex(self) -> String {
        let digit = |nibble: u8| char::from_digit(u32::from(nibble), 16).unwrap_or('0');
        self.0.iter().fold(String::with_capacity(64), |mut s, &b| {
            s.push(digit(b >> 4));
            s.push(digit(b & 0x0f));
            s
        })
    }
}

const fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl std::fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sha256Digest({})", self.to_hex())
    }
}

impl std::fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// A digest string that is not 64 hex characters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected 64 hex characters for a SHA-256 digest, got {len} bytes that do not parse")]
pub struct DigestParseError {
    len: usize,
}

impl Serialize for Sha256Digest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::from_hex(&raw).map_err(serde::de::Error::custom)
    }
}

/// How a model file's integrity is established at load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checksum {
    /// The file must hash to this digest — checked before the sessions are
    /// opened, again after, and on every replica rebuild.
    Sha256(Sha256Digest),
    /// No check. Local development only: logged at `warn` on every load and
    /// reported as [`super::ModelIntegrity::Unverified`].
    Skip,
}

/// One configured model: where its files live, its expected input/output
/// tensor contract, and how many session replicas to load (see
/// `docs/runtime/DESIGN.md` §2.2 and §4 decision 2, "Session replica pool").
///
/// # Wire format
///
/// Unchanged from the flat form operators already write: `sha256: "<hex>"`
/// pins the model, `skip_checksum: true` (local development only) skips the
/// check, and setting both or neither is a parse error. Both become
/// [`Self::checksum`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "ModelConfigWire", into = "ModelConfigWire")]
pub struct ModelConfig {
    /// Operator-chosen id. `[A-Za-z0-9][A-Za-z0-9._-]*`, at most
    /// [`MAX_MODEL_ID_LEN`] bytes, unique across `models`: it is a map key, a
    /// metric label and appears on the unauthenticated `/health`.
    pub model_id: String,
    /// Directory containing `model.onnx` (or the configured `model_file`
    /// name) and `tokenizer.json`.
    pub model_dir: PathBuf,
    /// A single file name inside `model_dir` — no separators, no `..`.
    pub model_file: String,
    /// A single file name inside `model_dir`.
    pub tokenizer_file: String,
    /// Optional metadata file carrying the model's `id2label` map. Absent for
    /// models that declare no fixed label set; never a load failure. A single
    /// file name inside `model_dir`.
    pub labels_file: String,
    /// How the model file is verified. See [`Checksum`].
    pub checksum: Checksum,
    /// Expected SHA-256 of the tokenizer file. Optional so existing configs
    /// keep loading, but a pinned model with an unpinned tokenizer is logged
    /// at `warn`: the tokenizer decides what the model sees.
    pub tokenizer_sha256: Option<Sha256Digest>,
    pub expected_inputs: Vec<String>,
    pub expected_outputs: Vec<String>,
    pub output_dtypes: BTreeMap<String, OutputDtype>,
    /// `1..=MAX_REPLICAS`.
    pub replicas: usize,
    /// `1..=MAX_INTRA_THREADS`.
    pub intra_threads: usize,

    // ---- Bounds -----------------------------------------------------------
    //
    // Defaults are chosen to be safe for an untrusted caller, because defaults
    // are what people actually deploy, and because the content flowing through
    // this gear is attacker-controlled by construction. They are generous
    // enough that today's consumer never sees them.
    /// Maximum elements in any one input tensor.
    pub max_tensor_elements: usize,
    /// Maximum input tensors in one `run` call.
    pub max_input_tensors: usize,
    /// Maximum extent of any single dimension.
    pub max_dimension: usize,
    /// Maximum UTF-8 bytes accepted by `encode`/`encode_fragment`. Oversized text
    /// is rejected, never silently truncated — truncation is a policy this
    /// layer deliberately does not own.
    pub max_text_bytes: usize,

    /// Wall-clock budget for one inference.
    ///
    /// On expiry the caller gets `Timeout`, but the work is *abandoned, not
    /// cancelled* — ONNX Runtime has no mid-graph cancellation, so the replica
    /// stays busy until the underlying call returns.
    pub inference_timeout: Duration,

    /// Concurrent inferences allowed for this model, at most
    /// [`MAX_IN_FLIGHT`]. Defaults to the replica count: more in flight than
    /// there are replicas is queueing, not parallelism, and unbounded queueing
    /// is what this cap exists to stop.
    pub max_in_flight: Option<NonZeroUsize>,

    /// What to do when `max_in_flight` is reached.
    pub on_overload: OverloadPolicy,
}

/// The flat form [`ModelConfig`] is written in. Kept private: the checked
/// type is the only one the rest of the gear sees.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelConfigWire {
    model_id: String,
    model_dir: PathBuf,
    #[serde(default = "default_model_file")]
    model_file: String,
    #[serde(default = "default_tokenizer_file")]
    tokenizer_file: String,
    #[serde(default = "default_labels_file")]
    labels_file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<Sha256Digest>,
    #[serde(default)]
    skip_checksum: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tokenizer_sha256: Option<Sha256Digest>,
    expected_inputs: Vec<String>,
    expected_outputs: Vec<String>,
    output_dtypes: BTreeMap<String, OutputDtype>,
    #[serde(default = "default_replicas")]
    replicas: usize,
    #[serde(default = "default_intra_threads")]
    intra_threads: usize,
    #[serde(default = "default_max_tensor_elements")]
    max_tensor_elements: usize,
    #[serde(default = "default_max_input_tensors")]
    max_input_tensors: usize,
    #[serde(default = "default_max_dimension")]
    max_dimension: usize,
    #[serde(default = "default_max_text_bytes")]
    max_text_bytes: usize,
    #[serde(default = "default_inference_timeout", with = "humantime_serde")]
    inference_timeout: Duration,
    #[serde(default)]
    max_in_flight: Option<NonZeroUsize>,
    #[serde(default)]
    on_overload: OverloadPolicy,
}

impl TryFrom<ModelConfigWire> for ModelConfig {
    type Error = String;

    fn try_from(w: ModelConfigWire) -> Result<Self, Self::Error> {
        let checksum = match (w.sha256, w.skip_checksum) {
            (Some(digest), false) => Checksum::Sha256(digest),
            (None, true) => Checksum::Skip,
            (None, false) => {
                return Err(format!(
                    "model {}: `sha256` is required (`skip_checksum: true` is for local \
                     development only)",
                    w.model_id
                ));
            }
            (Some(_), true) => {
                return Err(format!(
                    "model {}: set `sha256` or `skip_checksum: true`, not both",
                    w.model_id
                ));
            }
        };
        Ok(Self {
            model_id: w.model_id,
            model_dir: w.model_dir,
            model_file: w.model_file,
            tokenizer_file: w.tokenizer_file,
            labels_file: w.labels_file,
            checksum,
            tokenizer_sha256: w.tokenizer_sha256,
            expected_inputs: w.expected_inputs,
            expected_outputs: w.expected_outputs,
            output_dtypes: w.output_dtypes,
            replicas: w.replicas,
            intra_threads: w.intra_threads,
            max_tensor_elements: w.max_tensor_elements,
            max_input_tensors: w.max_input_tensors,
            max_dimension: w.max_dimension,
            max_text_bytes: w.max_text_bytes,
            inference_timeout: w.inference_timeout,
            max_in_flight: w.max_in_flight,
            on_overload: w.on_overload,
        })
    }
}

impl From<ModelConfig> for ModelConfigWire {
    fn from(c: ModelConfig) -> Self {
        let (sha256, skip_checksum) = match c.checksum {
            Checksum::Sha256(digest) => (Some(digest), false),
            Checksum::Skip => (None, true),
        };
        Self {
            model_id: c.model_id,
            model_dir: c.model_dir,
            model_file: c.model_file,
            tokenizer_file: c.tokenizer_file,
            labels_file: c.labels_file,
            sha256,
            skip_checksum,
            tokenizer_sha256: c.tokenizer_sha256,
            expected_inputs: c.expected_inputs,
            expected_outputs: c.expected_outputs,
            output_dtypes: c.output_dtypes,
            replicas: c.replicas,
            intra_threads: c.intra_threads,
            max_tensor_elements: c.max_tensor_elements,
            max_input_tensors: c.max_input_tensors,
            max_dimension: c.max_dimension,
            max_text_bytes: c.max_text_bytes,
            inference_timeout: c.inference_timeout,
            max_in_flight: c.max_in_flight,
            on_overload: c.on_overload,
        }
    }
}

impl Default for ModelConfig {
    /// Mirrors the serde defaults, so a test (or a caller building a config
    /// programmatically) can set the fields it cares about and inherit the
    /// rest rather than restating every bound.
    ///
    /// The default checksum is an all-zero digest that matches no real file:
    /// a programmatic caller must pin the real one or say `Checksum::Skip`
    /// out loud, never inherit "unchecked" by omission.
    fn default() -> Self {
        Self {
            model_id: String::new(),
            model_dir: PathBuf::new(),
            model_file: default_model_file(),
            tokenizer_file: default_tokenizer_file(),
            labels_file: default_labels_file(),
            checksum: Checksum::Sha256(Sha256Digest::from_bytes([0; 32])),
            tokenizer_sha256: None,
            expected_inputs: Vec::new(),
            expected_outputs: Vec::new(),
            output_dtypes: BTreeMap::new(),
            replicas: default_replicas(),
            intra_threads: default_intra_threads(),
            max_tensor_elements: default_max_tensor_elements(),
            max_input_tensors: default_max_input_tensors(),
            max_dimension: default_max_dimension(),
            max_text_bytes: default_max_text_bytes(),
            inference_timeout: default_inference_timeout(),
            max_in_flight: None,
            on_overload: OverloadPolicy::Queue,
        }
    }
}

/// A model configuration that cannot be served as written. Names the model
/// and the field; carries no file contents.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    #[error(
        "model id {model_id:?} is invalid: use 1-{MAX_MODEL_ID_LEN} characters from \
         [A-Za-z0-9._-], starting with a letter or digit"
    )]
    InvalidModelId { model_id: String },
    #[error("model id {model_id:?} is configured more than once")]
    DuplicateModelId { model_id: String },
    #[error("model {model_id}: `{field}` is {value}, must be between {min} and {max}")]
    OutOfRange {
        model_id: String,
        field: &'static str,
        value: u128,
        min: u128,
        max: u128,
    },
    #[error(
        "model {model_id}: `{field}` ({value:?}) must be a single file name inside `model_dir` \
         (no path separators, no `..`)"
    )]
    NotAFileName {
        model_id: String,
        field: &'static str,
        value: String,
    },
    #[error(
        "model {model_id}: output '{output}' is in `expected_outputs` but missing from \
         `output_dtypes`; every declared output needs a dtype, because guessing is how a \
         tensor gets reinterpreted"
    )]
    MissingOutputDtype { model_id: String, output: String },
}

/// Whether `model_id` matches `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
#[must_use]
pub fn is_valid_model_id(model_id: &str) -> bool {
    let mut bytes = model_id.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && model_id.len() <= MAX_MODEL_ID_LEN
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Checks every model id for shape and uniqueness. Cross-model, so it runs
/// once over the whole list rather than per model.
///
/// # Errors
///
/// The first invalid or repeated id.
pub fn validate_model_ids(models: &[ModelConfig]) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();
    for model in models {
        if !is_valid_model_id(&model.model_id) {
            return Err(ConfigError::InvalidModelId {
                model_id: model.model_id.clone(),
            });
        }
        if !seen.insert(model.model_id.as_str()) {
            return Err(ConfigError::DuplicateModelId {
                model_id: model.model_id.clone(),
            });
        }
    }
    Ok(())
}

/// `usize` widened for an error message; lossless on every real target.
fn wide(v: usize) -> u128 {
    u128::try_from(v).unwrap_or(u128::MAX)
}

fn is_single_file_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    ) && !name.contains(['/', '\\'])
}

impl ModelConfig {
    /// Checks every per-model bound before any file is read.
    ///
    /// # Errors
    ///
    /// The first field that cannot be served as written.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let range = |field: &'static str, value: usize, min: usize, max: usize| {
            if (min..=max).contains(&value) {
                Ok(())
            } else {
                Err(ConfigError::OutOfRange {
                    model_id: self.model_id.clone(),
                    field,
                    value: wide(value),
                    min: wide(min),
                    max: wide(max),
                })
            }
        };
        range("replicas", self.replicas, 1, MAX_REPLICAS)?;
        range("intra_threads", self.intra_threads, 1, MAX_INTRA_THREADS)?;
        if let Some(limit) = self.max_in_flight {
            range("max_in_flight", limit.get(), 1, MAX_IN_FLIGHT)?;
        }
        range(
            "max_tensor_elements",
            self.max_tensor_elements,
            1,
            usize::MAX,
        )?;
        range("max_input_tensors", self.max_input_tensors, 1, usize::MAX)?;
        range("max_dimension", self.max_dimension, 1, usize::MAX)?;
        range("max_text_bytes", self.max_text_bytes, 1, usize::MAX)?;
        if self.inference_timeout.is_zero() {
            return Err(ConfigError::OutOfRange {
                model_id: self.model_id.clone(),
                field: "inference_timeout",
                value: 0,
                min: 1,
                max: u128::MAX,
            });
        }

        for (field, value) in [
            ("model_file", &self.model_file),
            ("tokenizer_file", &self.tokenizer_file),
            ("labels_file", &self.labels_file),
        ] {
            if !is_single_file_name(value) {
                return Err(ConfigError::NotAFileName {
                    model_id: self.model_id.clone(),
                    field,
                    value: value.clone(),
                });
            }
        }

        // Checked before any I/O: a config that cannot produce a correct
        // result should not cost a 400 MB read first.
        if let Some(output) = self
            .expected_outputs
            .iter()
            .find(|output| !self.output_dtypes.contains_key(*output))
        {
            return Err(ConfigError::MissingOutputDtype {
                model_id: self.model_id.clone(),
                output: output.clone(),
            });
        }
        Ok(())
    }

    /// The effective in-flight ceiling: `max_in_flight`, or the replica count.
    #[must_use]
    pub fn effective_max_in_flight(&self) -> usize {
        self.max_in_flight.map_or(self.replicas, NonZeroUsize::get)
    }
}

/// What a caller gets when a model is already at capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverloadPolicy {
    /// Wait for a permit, bounded by `inference_timeout`. The default: a
    /// consumer's own deadline is usually a better place to give up than an
    /// arbitrary queue depth here, and the timeout already bounds the wait.
    #[default]
    Queue,
    /// Fail immediately with `Overloaded`. For deployments that would rather
    /// shed load than let it accumulate.
    Reject,
}

fn default_model_file() -> String {
    "model.onnx".to_owned()
}

fn default_labels_file() -> String {
    "config.json".to_owned()
}

fn default_tokenizer_file() -> String {
    "tokenizer.json".to_owned()
}

fn default_replicas() -> usize {
    4
}

fn default_intra_threads() -> usize {
    1
}

/// 16Mi elements — 64 MiB as f32, 128 MiB as i64. Far above any real
/// tokenized batch, far below "exhaust the host".
fn default_max_tensor_elements() -> usize {
    16 * 1024 * 1024
}

fn default_max_input_tensors() -> usize {
    32
}

fn default_max_dimension() -> usize {
    1024 * 1024
}

/// 1 MiB of text. Well past any prompt; a document belongs in an attachment
/// path that chunks it, not in a single `encode` call.
fn default_max_text_bytes() -> usize {
    1024 * 1024
}

/// Generous enough that a large model on a cold CPU is not cut off, tight
/// enough to shed a pathological input. Tune per model once
/// `observability` makes real latencies visible.
fn default_inference_timeout() -> Duration {
    Duration::from_secs(30)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::config::OnnxScannerConfig;

    /// The operator-facing reference config parses into the structs it claims to
    /// document, using the same duplicate-key-rejecting parser the server's config
    /// loader uses.
    ///
    /// This exists because a reference config is the one kind of documentation that
    /// can be wrong in a way nobody notices: it is never executed. `deny_unknown_fields`
    /// makes the test catch a renamed or removed field, which is exactly how a file
    /// like this rots.
    fn parse_example() -> OnnxScannerConfig {
        let raw = include_str!("../../docs/runtime/config-example.yaml");
        let opts = serde_saphyr::options! {
            duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        };
        serde_saphyr::from_str_with_options(raw, opts)
            .expect("docs/runtime/config-example.yaml must deserialize into OnnxScannerConfig")
    }

    fn parse_model(yaml: &str) -> Result<ModelConfig, String> {
        serde_saphyr::from_str::<ModelConfig>(yaml).map_err(|e| e.to_string())
    }

    const DIGEST: &str = "489edc522a34b8c6a0630c4710998abf0227b11acf45600ba2bce95bcd943930";

    fn minimal_yaml(extra: &str) -> String {
        format!(
            "model_id: m\nmodel_dir: /models/m\nexpected_inputs: [a]\nexpected_outputs: [b]\n\
             output_dtypes: {{b: f32}}\n{extra}"
        )
    }

    #[test]
    fn reference_config_parses() {
        let cfg = parse_example();

        assert!(!cfg.require_all_models);
        let ids: Vec<&str> = cfg.models.iter().map(|m| m.model_id.as_str()).collect();
        assert_eq!(ids, ["gliner2-pii", "piiranha-v1"]);
        assert!(validate_model_ids(&cfg.models).is_ok());
        for model in &cfg.models {
            assert_eq!(model.validate(), Ok(()), "{} validates", model.model_id);
        }
    }

    /// The annotated entry states a default in a comment beside almost every field.
    /// A comment claiming the wrong default is worse than no comment, so assert that
    /// the values written there are the values the code produces.
    #[test]
    fn annotated_entry_agrees_with_the_real_defaults() {
        let cfg = parse_example();
        let annotated = &cfg.models[0];
        let defaults = ModelConfig::default();

        assert_eq!(annotated.tokenizer_file, defaults.tokenizer_file);
        assert_eq!(annotated.labels_file, defaults.labels_file);
        assert_eq!(annotated.replicas, defaults.replicas);
        assert_eq!(annotated.intra_threads, defaults.intra_threads);
        assert_eq!(annotated.max_tensor_elements, defaults.max_tensor_elements);
        assert_eq!(annotated.max_input_tensors, defaults.max_input_tensors);
        assert_eq!(annotated.max_dimension, defaults.max_dimension);
        assert_eq!(annotated.max_text_bytes, defaults.max_text_bytes);
        assert_eq!(annotated.inference_timeout, defaults.inference_timeout);
        assert_eq!(annotated.on_overload, defaults.on_overload);

        // `max_in_flight` is the one knob written out rather than left implicit:
        // unset means "the replica count", and the file spells that out.
        assert_eq!(
            annotated.max_in_flight.map(NonZeroUsize::get),
            Some(annotated.replicas)
        );

        // Deliberately not defaults — the whole point of showing them.
        assert_eq!(annotated.model_file, "model.quantized.onnx");
        assert_ne!(annotated.model_file, defaults.model_file);
    }

    /// The minimal entry must actually be minimal: everything it omits has to come
    /// back as the documented default, or the file is teaching a fiction.
    #[test]
    fn minimal_entry_inherits_every_default() {
        let cfg = parse_example();
        let minimal = &cfg.models[1];
        let defaults = ModelConfig::default();

        assert_eq!(minimal.tokenizer_file, defaults.tokenizer_file);
        assert_eq!(minimal.labels_file, defaults.labels_file);
        assert_eq!(minimal.replicas, defaults.replicas);
        assert_eq!(minimal.intra_threads, defaults.intra_threads);
        assert_eq!(minimal.inference_timeout, defaults.inference_timeout);
        assert_eq!(minimal.max_in_flight, None);
        assert_eq!(minimal.on_overload, OverloadPolicy::Queue);
        assert!(matches!(minimal.checksum, Checksum::Sha256(_)));
    }

    /// Both entries pin a digest. A reference config that shipped `skip_checksum:
    /// true` would be teaching operators to disable the only artifact-integrity
    /// control the gear has.
    #[test]
    fn reference_config_never_disables_integrity_checking() {
        for model in parse_example().models {
            assert!(
                matches!(model.checksum, Checksum::Sha256(_)),
                "{} skips its checksum in the reference config",
                model.model_id
            );
        }
    }

    /// Every declared output needs a dtype. This is a startup failure in the real
    /// loader; the reference config must not model the failure.
    #[test]
    fn every_declared_output_has_a_dtype() {
        for model in parse_example().models {
            for output in &model.expected_outputs {
                assert!(
                    model.output_dtypes.contains_key(output),
                    "{} declares output {output} with no dtype",
                    model.model_id
                );
            }
            assert_eq!(model.output_dtypes.len(), model.expected_outputs.len());
        }
    }

    // --- Checksum wire format ---------------------------------------------

    #[test]
    fn sha256_parses_into_a_pinned_checksum() {
        let cfg = parse_model(&minimal_yaml(&format!("sha256: \"{DIGEST}\"\n"))).expect("parses");
        let Checksum::Sha256(digest) = cfg.checksum else {
            panic!("expected a pinned digest, got {:?}", cfg.checksum);
        };
        assert_eq!(digest.to_hex(), DIGEST);
    }

    #[test]
    fn uppercase_hex_is_accepted_and_normalized() {
        let upper = DIGEST.to_ascii_uppercase();
        let cfg = parse_model(&minimal_yaml(&format!("sha256: \"{upper}\"\n"))).expect("parses");
        assert_eq!(
            cfg.checksum,
            Checksum::Sha256(Sha256Digest::from_hex(DIGEST).expect("valid"))
        );
    }

    #[test]
    fn skip_checksum_parses_into_skip() {
        let cfg = parse_model(&minimal_yaml("skip_checksum: true\n")).expect("parses");
        assert_eq!(cfg.checksum, Checksum::Skip);
    }

    #[test]
    fn neither_digest_nor_skip_is_rejected() {
        let err = parse_model(&minimal_yaml("")).expect_err("must not parse");
        assert!(err.contains("sha256"), "names the field: {err}");
    }

    #[test]
    fn both_digest_and_skip_is_rejected() {
        let err = parse_model(&minimal_yaml(&format!(
            "sha256: \"{DIGEST}\"\nskip_checksum: true\n"
        )))
        .expect_err("ambiguous");
        assert!(err.contains("not both"), "{err}");
    }

    #[test]
    fn malformed_digests_are_rejected_at_parse() {
        for bad in ["", "abc", &"g".repeat(64), &"a".repeat(63), &"a".repeat(65)] {
            assert!(
                parse_model(&minimal_yaml(&format!("sha256: \"{bad}\"\n"))).is_err(),
                "{bad:?} must not parse as a digest"
            );
            assert!(Sha256Digest::from_hex(bad).is_err());
        }
    }

    #[test]
    fn digest_hex_round_trips() {
        let digest = Sha256Digest::from_hex(DIGEST).expect("valid");
        assert_eq!(Sha256Digest::from_hex(&digest.to_hex()), Ok(digest));
        assert_eq!(digest.to_string(), DIGEST);
    }

    #[test]
    fn config_round_trips_through_the_wire_form() {
        let cfg = parse_model(&minimal_yaml(&format!(
            "sha256: \"{DIGEST}\"\ntokenizer_sha256: \"{DIGEST}\"\nmax_in_flight: 3\n"
        )))
        .expect("parses");
        let json = serde_json::to_string(&cfg).expect("serializes");
        assert!(json.contains(DIGEST), "{json}");
        assert!(json.contains("\"skip_checksum\":false"), "{json}");
        let back: ModelConfig = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back.checksum, cfg.checksum);
        assert_eq!(back.tokenizer_sha256, cfg.tokenizer_sha256);
        assert_eq!(back.max_in_flight, NonZeroUsize::new(3));
    }

    #[test]
    fn zero_max_in_flight_is_rejected_at_parse() {
        assert!(
            parse_model(&minimal_yaml(&format!(
                "sha256: \"{DIGEST}\"\nmax_in_flight: 0\n"
            )))
            .is_err()
        );
    }

    #[test]
    fn the_default_checksum_matches_no_file_rather_than_skipping() {
        assert!(matches!(
            ModelConfig::default().checksum,
            Checksum::Sha256(_)
        ));
    }

    // --- Validation -------------------------------------------------------

    fn valid() -> ModelConfig {
        ModelConfig {
            model_id: "m".to_owned(),
            ..ModelConfig::default()
        }
    }

    fn rejected_field(cfg: &ModelConfig) -> &'static str {
        match cfg.validate() {
            Err(
                ConfigError::OutOfRange { field, .. } | ConfigError::NotAFileName { field, .. },
            ) => field,
            other => panic!("expected a field rejection, got {other:?}"),
        }
    }

    #[test]
    fn the_defaults_validate() {
        assert_eq!(valid().validate(), Ok(()));
    }

    #[test]
    fn bounds_are_inclusive_at_both_ends() {
        for replicas in [1, MAX_REPLICAS] {
            assert_eq!(
                ModelConfig {
                    replicas,
                    ..valid()
                }
                .validate(),
                Ok(())
            );
        }
        for intra_threads in [1, MAX_INTRA_THREADS] {
            assert_eq!(
                ModelConfig {
                    intra_threads,
                    ..valid()
                }
                .validate(),
                Ok(())
            );
        }
        for limit in [1, MAX_IN_FLIGHT] {
            let cfg = ModelConfig {
                max_in_flight: NonZeroUsize::new(limit),
                ..valid()
            };
            assert_eq!(cfg.validate(), Ok(()));
        }
    }

    #[test]
    fn out_of_range_tunables_name_their_field() {
        let cases: [(ModelConfig, &str); 9] = [
            (
                ModelConfig {
                    replicas: 0,
                    ..valid()
                },
                "replicas",
            ),
            (
                ModelConfig {
                    replicas: MAX_REPLICAS + 1,
                    ..valid()
                },
                "replicas",
            ),
            (
                ModelConfig {
                    intra_threads: 0,
                    ..valid()
                },
                "intra_threads",
            ),
            (
                ModelConfig {
                    intra_threads: MAX_INTRA_THREADS + 1,
                    ..valid()
                },
                "intra_threads",
            ),
            (
                ModelConfig {
                    max_in_flight: NonZeroUsize::new(MAX_IN_FLIGHT + 1),
                    ..valid()
                },
                "max_in_flight",
            ),
            (
                ModelConfig {
                    max_tensor_elements: 0,
                    ..valid()
                },
                "max_tensor_elements",
            ),
            (
                ModelConfig {
                    max_input_tensors: 0,
                    ..valid()
                },
                "max_input_tensors",
            ),
            (
                ModelConfig {
                    max_text_bytes: 0,
                    ..valid()
                },
                "max_text_bytes",
            ),
            (
                ModelConfig {
                    inference_timeout: Duration::ZERO,
                    ..valid()
                },
                "inference_timeout",
            ),
        ];
        for (cfg, field) in cases {
            assert_eq!(rejected_field(&cfg), field);
            let message = cfg.validate().expect_err("rejected").to_string();
            assert!(message.contains(field), "{message}");
            assert!(message.contains("model m"), "{message}");
        }
    }

    #[test]
    fn file_names_must_stay_inside_the_model_dir() {
        for bad in [
            "",
            "..",
            ".",
            "../model.onnx",
            "/etc/passwd",
            "a/b.onnx",
            "a\\b.onnx",
        ] {
            for field in ["model_file", "tokenizer_file", "labels_file"] {
                let mut cfg = valid();
                match field {
                    "model_file" => cfg.model_file = bad.to_owned(),
                    "tokenizer_file" => cfg.tokenizer_file = bad.to_owned(),
                    _ => cfg.labels_file = bad.to_owned(),
                }
                assert_eq!(rejected_field(&cfg), field, "{bad:?} accepted for {field}");
            }
        }
        let cfg = ModelConfig {
            model_file: "model.quantized.onnx".to_owned(),
            ..valid()
        };
        assert_eq!(cfg.validate(), Ok(()));
    }

    #[test]
    fn an_output_without_a_dtype_is_rejected() {
        let cfg = ModelConfig {
            expected_outputs: vec!["logits".to_owned()],
            ..valid()
        };
        assert!(matches!(
            cfg.validate(),
            Err(ConfigError::MissingOutputDtype { ref output, .. }) if output == "logits"
        ));
    }

    #[test]
    fn model_ids_follow_the_allowlist() {
        for good in [
            "m",
            "gliner2-pii",
            "piiranha-v1",
            "A.b_c-9",
            &"a".repeat(64),
        ] {
            assert!(is_valid_model_id(good), "{good:?} should be accepted");
        }
        for bad in [
            "",
            "-leading",
            ".hidden",
            "has space",
            "slash/y",
            "new\nline",
            "\u{fc}n\u{ef}code",
            &"a".repeat(65),
        ] {
            assert!(!is_valid_model_id(bad), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn duplicate_and_malformed_ids_are_rejected_across_the_list() {
        let named = |id: &str| ModelConfig {
            model_id: id.to_owned(),
            ..ModelConfig::default()
        };
        assert_eq!(validate_model_ids(&[named("a"), named("b")]), Ok(()));
        assert_eq!(
            validate_model_ids(&[named("a"), named("b"), named("a")]),
            Err(ConfigError::DuplicateModelId {
                model_id: "a".to_owned()
            })
        );
        assert_eq!(
            validate_model_ids(&[named("ok"), named("not ok")]),
            Err(ConfigError::InvalidModelId {
                model_id: "not ok".to_owned()
            })
        );
    }

    #[test]
    fn effective_max_in_flight_defaults_to_replicas() {
        assert_eq!(
            ModelConfig {
                replicas: 3,
                ..valid()
            }
            .effective_max_in_flight(),
            3
        );
        let capped = ModelConfig {
            replicas: 3,
            max_in_flight: NonZeroUsize::new(7),
            ..valid()
        };
        assert_eq!(capped.effective_max_in_flight(), 7);
    }
}
