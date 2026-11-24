use serde::{Deserialize, Serialize};

use crate::domain::Thresholds;
use crate::runtime::config::{MAX_MODEL_ID_LEN, ModelConfig, is_valid_model_id};

/// Which PII model family `onnx-scanner` runs, and that family's own
/// config — see `domain::backend::PiiBackend` for why this is a trait
/// rather than a hardcoded model.
///
/// `model_id` must match an entry of [`OnnxScannerConfig::models`]. Models are
/// operator-provisioned files, verified at start; this plugin never fetches
/// one — see `docs/runtime/DESIGN.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum PiiBackendConfig {
    /// A plain BIO token-classification model (`minilm-l6-pii` by
    /// default).
    BioTokenClassifier {
        #[serde(default = "default_bio_model_id")]
        model_id: String,
        /// BIO label set, in the exact order the model's output logits are
        /// indexed (e.g. `["O", "B-EMAIL", "I-EMAIL", ...]`). When the model
        /// publishes an `id2label` map, it must match this list exactly
        /// (checked at init).
        #[serde(default = "default_bio_labels")]
        labels: Vec<String>,
    },
    /// Fastino's `GLiNER2`, a zero-shot span model specialized for PII — see
    /// `domain::gliner2`'s module docs.
    Gliner2 {
        #[serde(default = "default_gliner2_model_id")]
        model_id: String,
        /// Entity labels to condition the schema prompt on (e.g.
        /// `["person", "email", ...]`) — also used directly as
        /// `Finding.category` (lowercase, no further mapping).
        #[serde(default = "default_gliner2_labels")]
        labels: Vec<String>,
        /// Spans scoring below this are never emitted, independent of
        /// `redact_threshold`/`warn_threshold` below (which only ever see
        /// scores this backend actually reports). The model card's
        /// evaluated operating points are 0.3/0.5/0.7. Must be in `(0, 1]`.
        #[serde(default = "default_gliner2_decode_threshold")]
        decode_threshold: f32,
    },
}

impl Default for PiiBackendConfig {
    fn default() -> Self {
        Self::BioTokenClassifier {
            model_id: default_bio_model_id(),
            labels: default_bio_labels(),
        }
    }
}

impl PiiBackendConfig {
    /// The model this backend runs.
    #[must_use]
    pub fn model_id(&self) -> &str {
        match self {
            Self::BioTokenClassifier { model_id, .. } | Self::Gliner2 { model_id, .. } => model_id,
        }
    }

    /// The configured label list.
    #[must_use]
    pub fn labels(&self) -> &[String] {
        match self {
            Self::BioTokenClassifier { labels, .. } | Self::Gliner2 { labels, .. } => labels,
        }
    }
}

/// Configuration for the `onnx-scanner` gear.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnnxScannerConfig {
    #[serde(default = "default_vendor")]
    pub vendor: String,
    #[serde(default = "default_priority")]
    pub priority: i16,
    #[serde(default)]
    pub backend: PiiBackendConfig,
    /// The models this plugin runs: where each lives, its tensor contract, its
    /// pinned SHA-256 and its bounds. See `docs/runtime/config-example.yaml`.
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    /// Fail startup if any configured model cannot be loaded.
    ///
    /// Defaults to `false`: a bad artifact makes *that model* unavailable and
    /// leaves the rest serving, because this plugin shares a process with every
    /// other gear and must not take them down with it. The backend reports an
    /// unavailable model as `BackendUnavailable`: `prompt-scanner` then fails
    /// the scan when the tenant policy requires this backend, and otherwise
    /// lists it in the verdict's `unscored_backends` (reason `unavailable`),
    /// logs and counts it — a scan that could not run is not a clean scan.
    /// That is what makes the permissive default safe rather than merely
    /// convenient. Set `true` for deployments that would rather not start at
    /// all than start degraded.
    #[serde(default)]
    pub require_all_models: bool,
    /// Prefix for `Finding.category` (e.g. `"pii"` -> `"pii.email"`).
    #[serde(default = "default_category_prefix")]
    pub category_prefix: String,
    /// Payload word count above which `detect` refuses to scan and returns an
    /// error instead of a result — see `domain::service`'s module docs for why
    /// this is an error rather than a deferral. At least
    /// [`MIN_MAX_WORDS`]. Accepts the old key `defer_threshold_words`.
    #[serde(default = "default_max_words", alias = "defer_threshold_words")]
    pub max_words: usize,
    /// Payload size in bytes above which `detect` refuses to scan. Bounds the
    /// cost of inputs with few or no word breaks, which the word limit cannot
    /// see. Must be in `1..=`[`prompt_scanner_sdk::MAX_PAYLOAD_BYTES`].
    #[serde(default = "default_max_payload_bytes")]
    pub max_payload_bytes: usize,
    /// Confidence at/above which a finding suggests `VerdictAction::Redact`.
    /// In `[0, 1]`, and at least `warn_threshold`.
    #[serde(default = "default_redact_threshold")]
    pub redact_threshold: f32,
    /// Confidence at/above which a finding suggests `VerdictAction::Warn`
    /// (ignored once `redact_threshold` is reached). In `[0, 1]`.
    #[serde(default = "default_warn_threshold")]
    pub warn_threshold: f32,
}

impl Default for OnnxScannerConfig {
    fn default() -> Self {
        Self {
            vendor: default_vendor(),
            priority: default_priority(),
            backend: PiiBackendConfig::default(),
            models: Vec::new(),
            require_all_models: false,
            category_prefix: default_category_prefix(),
            max_words: default_max_words(),
            max_payload_bytes: default_max_payload_bytes(),
            redact_threshold: default_redact_threshold(),
            warn_threshold: default_warn_threshold(),
        }
    }
}

/// The smallest accepted [`OnnxScannerConfig::max_words`]. Lower values make
/// the plugin refuse ordinary prompts, which is a misconfiguration rather than
/// a policy.
pub const MIN_MAX_WORDS: usize = 64;

/// A configuration value the plugin cannot run with.
#[derive(Debug, thiserror::Error)]
#[error("onnx-scanner config: `{field}` {problem}")]
pub struct ConfigError {
    /// The offending field, as written in YAML.
    pub field: &'static str,
    /// What is wrong with it.
    pub problem: String,
}

impl ConfigError {
    fn new(field: &'static str, problem: impl Into<String>) -> Self {
        Self {
            field,
            problem: problem.into(),
        }
    }
}

impl OnnxScannerConfig {
    /// Checks every tunable before anything is loaded, naming the field at
    /// fault.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] for a threshold out of range or out of order, a word or
    /// byte limit out of range, a malformed `model_id`, a `model_id` with no
    /// entry in `models`, or an empty label list.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.thresholds()?;
        if self.max_words < MIN_MAX_WORDS {
            return Err(ConfigError::new(
                "max_words",
                format!("must be at least {MIN_MAX_WORDS}, got {}", self.max_words),
            ));
        }
        if self.max_payload_bytes == 0
            || self.max_payload_bytes > prompt_scanner_sdk::MAX_PAYLOAD_BYTES
        {
            return Err(ConfigError::new(
                "max_payload_bytes",
                format!(
                    "must be in 1..={}, got {}",
                    prompt_scanner_sdk::MAX_PAYLOAD_BYTES,
                    self.max_payload_bytes
                ),
            ));
        }
        if self.category_prefix.trim().is_empty() {
            return Err(ConfigError::new("category_prefix", "must not be empty"));
        }

        let model_id = self.backend.model_id();
        validate_model_id(model_id)?;
        if !self.models.iter().any(|m| m.model_id == model_id) {
            return Err(ConfigError::new(
                "backend.model_id",
                format!("`{model_id}` has no entry in `models`"),
            ));
        }
        let labels = self.backend.labels();
        if labels.is_empty() || labels.iter().any(|l| l.trim().is_empty()) {
            return Err(ConfigError::new(
                "backend.labels",
                "must be a non-empty list of non-empty labels",
            ));
        }
        if let PiiBackendConfig::Gliner2 {
            decode_threshold, ..
        } = &self.backend
            && !(decode_threshold.is_finite()
                && *decode_threshold > 0.0
                && *decode_threshold <= 1.0)
        {
            return Err(ConfigError::new(
                "backend.decode_threshold",
                format!("must be in (0, 1], got {decode_threshold}"),
            ));
        }
        Ok(())
    }

    /// The validated redact/warn pair.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] when either is not a number in `[0, 1]`, or
    /// `warn_threshold > redact_threshold`.
    pub fn thresholds(&self) -> Result<Thresholds, ConfigError> {
        Thresholds::new(self.redact_threshold, self.warn_threshold)
            .map_err(|problem| ConfigError::new("redact_threshold/warn_threshold", problem))
    }

    /// The `models` entry the backend runs. `None` only for an unvalidated
    /// config.
    #[must_use]
    pub fn backend_model(&self) -> Option<&ModelConfig> {
        let id = self.backend.model_id();
        self.models.iter().find(|m| m.model_id == id)
    }
}

/// Model ids reach logs and the unauthenticated health report, so the
/// backend's id must be well formed by the model host's own rule.
fn validate_model_id(model_id: &str) -> Result<(), ConfigError> {
    if is_valid_model_id(model_id) {
        Ok(())
    } else {
        Err(ConfigError::new(
            "backend.model_id",
            format!(
                "must be 1..={MAX_MODEL_ID_LEN} ASCII letters, digits, `-`, `_` or `.`, \
                 starting with a letter or digit"
            ),
        ))
    }
}

fn default_vendor() -> String {
    "cf.builtin.onnx_pii".to_owned()
}

fn default_priority() -> i16 {
    1000
}

fn default_bio_model_id() -> String {
    "minilm-l6-pii".to_owned()
}

/// `Negative-Star-Innovators/MiniLM-L6-finetuned-pii-detection-v2`'s label
/// set (see the model card). Operators pointing this plugin at a different BIO
/// model must override this to match.
fn default_bio_labels() -> Vec<String> {
    [
        "O",
        "B-EMAIL",
        "I-EMAIL",
        "B-PHONE",
        "I-PHONE",
        "B-PERSON",
        "I-PERSON",
        "B-ADDRESS",
        "I-ADDRESS",
        "B-DATE",
        "I-DATE",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

fn default_gliner2_model_id() -> String {
    "gliner2-pii".to_owned()
}

/// The label schema `dlp-bench` validated as `GLINER2_PII` — see that
/// repo's `src/labels.rs`.
fn default_gliner2_labels() -> Vec<String> {
    [
        "person",
        "email",
        "phone_number",
        "address",
        "date_of_birth",
        "national_id_number",
        "passport_number",
        "drivers_license_number",
        "card_number",
        "bank_account",
        "iban",
        "ip_address",
        "username",
        "api_key",
        "access_token",
        "password",
        "secret",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

fn default_gliner2_decode_threshold() -> f32 {
    0.5
}

fn default_category_prefix() -> String {
    "pii".to_owned()
}

fn default_max_words() -> usize {
    2_000
}

/// 1 MiB, the same as the model host's default `max_text_bytes`.
fn default_max_payload_bytes() -> usize {
    1024 * 1024
}

fn default_redact_threshold() -> f32 {
    0.6
}

fn default_warn_threshold() -> f32 {
    0.3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> ModelConfig {
        ModelConfig {
            model_id: id.to_owned(),
            ..Default::default()
        }
    }

    fn valid() -> OnnxScannerConfig {
        OnnxScannerConfig {
            models: vec![model("minilm-l6-pii")],
            ..Default::default()
        }
    }

    fn field_of(cfg: &OnnxScannerConfig) -> &'static str {
        cfg.validate().expect_err("must be rejected").field
    }

    #[test]
    fn defaults_are_pinned() {
        let cfg = OnnxScannerConfig::default();
        assert_eq!(cfg.vendor, "cf.builtin.onnx_pii");
        assert_eq!(cfg.priority, 1000);
        assert_eq!(cfg.category_prefix, "pii");
        assert_eq!(cfg.max_words, 2_000);
        assert_eq!(cfg.max_payload_bytes, 1024 * 1024);
        assert!((cfg.redact_threshold - 0.6).abs() < f32::EPSILON);
        assert!((cfg.warn_threshold - 0.3).abs() < f32::EPSILON);
        assert!(!cfg.require_all_models);
        assert_eq!(cfg.backend.model_id(), "minilm-l6-pii");
        assert_eq!(cfg.backend.labels().len(), 11);
    }

    #[test]
    fn a_config_with_its_model_listed_is_valid() {
        valid().validate().expect("valid");
    }

    #[test]
    fn default_config_without_models_is_rejected() {
        assert_eq!(
            field_of(&OnnxScannerConfig::default()),
            "backend.model_id",
            "a scanner whose model is not configured would fail every scan"
        );
    }

    #[test]
    fn thresholds_out_of_range_or_order_are_rejected() {
        for (redact, warn) in [
            (f32::NAN, 0.3),
            (0.6, f32::NAN),
            (1.5, 0.3),
            (0.6, -0.1),
            (0.2, 0.3),
            (f32::INFINITY, 0.3),
        ] {
            let cfg = OnnxScannerConfig {
                redact_threshold: redact,
                warn_threshold: warn,
                ..valid()
            };
            assert_eq!(
                field_of(&cfg),
                "redact_threshold/warn_threshold",
                "{redact}/{warn}"
            );
        }
        // Equal is allowed: every finding at that score redacts.
        let cfg = OnnxScannerConfig {
            redact_threshold: 0.5,
            warn_threshold: 0.5,
            ..valid()
        };
        cfg.validate().expect("warn == redact is valid");
    }

    #[test]
    fn word_and_byte_limits_are_bounded() {
        let cfg = OnnxScannerConfig {
            max_words: 0,
            ..valid()
        };
        assert_eq!(field_of(&cfg), "max_words");
        let cfg = OnnxScannerConfig {
            max_words: MIN_MAX_WORDS - 1,
            ..valid()
        };
        assert_eq!(field_of(&cfg), "max_words");
        let cfg = OnnxScannerConfig {
            max_words: MIN_MAX_WORDS,
            ..valid()
        };
        cfg.validate().expect("the minimum itself is valid");

        let cfg = OnnxScannerConfig {
            max_payload_bytes: 0,
            ..valid()
        };
        assert_eq!(field_of(&cfg), "max_payload_bytes");
        let cfg = OnnxScannerConfig {
            max_payload_bytes: prompt_scanner_sdk::MAX_PAYLOAD_BYTES + 1,
            ..valid()
        };
        assert_eq!(field_of(&cfg), "max_payload_bytes");
    }

    #[test]
    fn malformed_model_ids_are_rejected() {
        for id in [
            "",
            "../etc",
            "a b",
            "<script>",
            "-lead",
            &"x".repeat(MAX_MODEL_ID_LEN + 1),
        ] {
            let cfg = OnnxScannerConfig {
                backend: PiiBackendConfig::BioTokenClassifier {
                    model_id: id.to_owned(),
                    labels: default_bio_labels(),
                },
                models: vec![model(id)],
                ..Default::default()
            };
            assert_eq!(field_of(&cfg), "backend.model_id", "{id:?}");
        }
    }

    #[test]
    fn empty_labels_are_rejected() {
        let cfg = OnnxScannerConfig {
            backend: PiiBackendConfig::BioTokenClassifier {
                model_id: "minilm-l6-pii".to_owned(),
                labels: Vec::new(),
            },
            ..valid()
        };
        assert_eq!(field_of(&cfg), "backend.labels");
    }

    #[test]
    fn gliner2_decode_threshold_is_bounded() {
        for threshold in [0.0, -0.5, 1.01, f32::NAN] {
            let cfg = OnnxScannerConfig {
                backend: PiiBackendConfig::Gliner2 {
                    model_id: "gliner2-pii".to_owned(),
                    labels: default_gliner2_labels(),
                    decode_threshold: threshold,
                },
                models: vec![model("gliner2-pii")],
                ..Default::default()
            };
            assert_eq!(field_of(&cfg), "backend.decode_threshold", "{threshold}");
        }
    }

    #[test]
    fn old_defer_key_still_parses() {
        let cfg: OnnxScannerConfig =
            serde_json::from_str(r#"{"defer_threshold_words": 500}"#).expect("parses");
        assert_eq!(cfg.max_words, 500);
    }
}
