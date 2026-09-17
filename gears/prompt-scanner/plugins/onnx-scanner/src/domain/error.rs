//! The domain's own error type, and the one place it becomes a
//! [`PromptScannerError`].
//!
//! Backends report what went wrong in domain terms ([`DomainError`]); only
//! [`DomainError::into_scanner_error`] decides what crosses the plugin
//! boundary. That keeps two rules in one spot:
//!
//! - **Classification survives.** A runtime timeout becomes
//!   [`PromptScannerError::BackendTimeout`], load shedding and an unavailable
//!   model become [`PromptScannerError::BackendUnavailable`] with a stable
//!   reason code, a caller-side input problem becomes
//!   [`PromptScannerError::Validation`], and everything else is
//!   [`PromptScannerError::Internal`].
//! - **No raw error text on the wire.** Runtime messages can carry ONNX
//!   Runtime text, panic payloads and file paths. The full error is logged at
//!   `warn` with the `model_id`; the caller only sees a stable code.

use prompt_scanner_sdk::PromptScannerError;
use thiserror::Error;

use crate::runtime::RuntimeError;

/// The backend name reported in every error this plugin returns.
pub const BACKEND_NAME: &str = "onnx-scanner";

/// Stable reason codes carried by [`PromptScannerError::BackendUnavailable`].
pub mod reason {
    /// The model failed to load or lost its sessions.
    pub const MODEL_UNAVAILABLE: &str = "model_unavailable";
    /// The model is at capacity; retrying later may succeed.
    pub const OVERLOADED: &str = "overloaded";
    /// Inference itself failed inside ONNX Runtime.
    pub const INFERENCE_FAILED: &str = "inference_failed";
}

/// Why a PII backend could not produce entities.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DomainError {
    /// The model host refused or failed the call.
    #[error("model runtime call failed for model {model_id}")]
    Runtime {
        model_id: String,
        #[source]
        source: RuntimeError,
    },

    /// The configured model is not loaded (absent from `list_models`, or
    /// present without a loaded session).
    #[error("model {model_id} is not loaded")]
    ModelNotLoaded { model_id: String },

    /// The model's outputs do not match what the decoder expects: a missing
    /// output, a wrong dtype, or a shape that disagrees with the label set or
    /// the sequence length. Never decoded as "no entities".
    #[error("model {model_id} output contract violated: {detail}")]
    OutputContract { model_id: String, detail: String },

    /// The tokenizer behind the model is not the one the backend was built
    /// against (special-token ids differ).
    #[error("model {model_id} tokenizer mismatch: {detail}")]
    TokenizerMismatch { model_id: String, detail: String },

    /// The scanned text is larger than this backend accepts.
    #[error("input of {bytes} bytes exceeds the {limit}-byte limit")]
    InputTooLarge { bytes: usize, limit: usize },

    /// The scanned text has more words than this plugin scans in one call.
    #[error("input exceeds the {limit}-word limit")]
    InputTooManyWords { limit: usize },

    /// A backend was configured in a way it cannot run with.
    #[error("invalid backend configuration: {0}")]
    InvalidConfig(String),
}

impl DomainError {
    pub fn runtime(model_id: &str, source: RuntimeError) -> Self {
        Self::Runtime {
            model_id: model_id.to_owned(),
            source,
        }
    }

    pub fn contract(model_id: &str, detail: impl Into<String>) -> Self {
        Self::OutputContract {
            model_id: model_id.to_owned(),
            detail: detail.into(),
        }
    }

    fn model_id(&self) -> Option<&str> {
        match self {
            Self::Runtime { model_id, .. }
            | Self::ModelNotLoaded { model_id }
            | Self::OutputContract { model_id, .. }
            | Self::TokenizerMismatch { model_id, .. } => Some(model_id),
            Self::InputTooLarge { .. }
            | Self::InputTooManyWords { .. }
            | Self::InvalidConfig(_) => None,
        }
    }

    /// The error a caller of the plugin sees. Logs the full error (with its
    /// source chain) at `warn`, because what is returned deliberately carries
    /// no detail.
    #[must_use]
    pub fn into_scanner_error(self) -> PromptScannerError {
        tracing::warn!(
            model_id = self.model_id().unwrap_or(""),
            error = &self as &dyn std::error::Error,
            "onnx-scanner: PII detection failed"
        );
        self.to_wire()
    }

    /// The wire mapping, without the log line. Separate so it is testable.
    fn to_wire(&self) -> PromptScannerError {
        let unavailable = |code: &str| PromptScannerError::BackendUnavailable {
            backend: BACKEND_NAME.to_owned(),
            reason: code.to_owned(),
        };
        match self {
            Self::Runtime { source, .. } => match source {
                RuntimeError::Timeout { .. } => PromptScannerError::backend_timeout(BACKEND_NAME),
                RuntimeError::Overloaded { .. } => unavailable(reason::OVERLOADED),
                RuntimeError::ModelUnavailable { .. } => unavailable(reason::MODEL_UNAVAILABLE),
                RuntimeError::InferenceFailed { .. } => unavailable(reason::INFERENCE_FAILED),
                // The only caller-side runtime failure: the scanned input was
                // rejected by the model's bounds before any work started.
                RuntimeError::InvalidInput { .. } => PromptScannerError::validation(
                    "onnx-scanner: input exceeds the model's input bounds",
                ),
                _ => PromptScannerError::internal("onnx-scanner: model runtime error"),
            },
            Self::ModelNotLoaded { .. } => unavailable(reason::MODEL_UNAVAILABLE),
            Self::InputTooLarge { limit, .. } => PromptScannerError::validation(format!(
                "onnx-scanner: input exceeds the {limit}-byte limit"
            )),
            Self::InputTooManyWords { limit } => PromptScannerError::validation(format!(
                "onnx-scanner: input exceeds the {limit}-word limit"
            )),
            Self::OutputContract { .. } => {
                PromptScannerError::internal("onnx-scanner: model output contract violated")
            }
            Self::TokenizerMismatch { .. } => {
                PromptScannerError::internal("onnx-scanner: tokenizer does not match the model")
            }
            Self::InvalidConfig(_) => {
                PromptScannerError::internal("onnx-scanner: invalid backend configuration")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ModelFailureReason;

    fn wire(source: RuntimeError) -> PromptScannerError {
        DomainError::runtime("m", source).to_wire()
    }

    const SECRET: &str = "/srv/models/secret-path panic: boom";

    #[test]
    fn timeout_maps_to_backend_timeout() {
        let e = wire(RuntimeError::Timeout {
            model_id: "m".to_owned(),
            elapsed_ms: 5,
        });
        assert!(
            matches!(e, PromptScannerError::BackendTimeout { .. }),
            "{e:?}"
        );
    }

    #[test]
    fn overload_and_unavailable_map_to_stable_codes() {
        let e = wire(RuntimeError::Overloaded {
            model_id: "m".to_owned(),
            max_in_flight: 3,
        });
        assert!(
            matches!(&e, PromptScannerError::BackendUnavailable { reason, .. } if reason == reason::OVERLOADED),
            "{e:?}"
        );
        let e = wire(RuntimeError::ModelUnavailable {
            model_id: "m".to_owned(),
            reason: ModelFailureReason::SessionBuildFailed,
        });
        assert!(
            matches!(&e, PromptScannerError::BackendUnavailable { reason, .. } if reason == reason::MODEL_UNAVAILABLE),
            "{e:?}"
        );
    }

    #[test]
    fn invalid_input_is_a_caller_error() {
        let e = wire(RuntimeError::InvalidInput {
            model_id: "m".to_owned(),
            tensor: "input_ids".to_owned(),
            message: SECRET.to_owned(),
        });
        assert!(matches!(e, PromptScannerError::Validation { .. }), "{e:?}");
    }

    #[test]
    fn raw_runtime_text_never_reaches_the_wire() {
        for source in [
            RuntimeError::InferenceFailed {
                model_id: "m".to_owned(),
                message: SECRET.to_owned(),
            },
            RuntimeError::Internal(SECRET.to_owned()),
            RuntimeError::ContractMismatch {
                model_id: "m".to_owned(),
                message: SECRET.to_owned(),
            },
            RuntimeError::InvalidInput {
                model_id: "m".to_owned(),
                tensor: "t".to_owned(),
                message: SECRET.to_owned(),
            },
        ] {
            let shown = wire(source).to_string();
            assert!(!shown.contains("secret-path"), "leaked: {shown}");
            assert!(!shown.contains("boom"), "leaked: {shown}");
        }
        let shown = DomainError::contract("m", SECRET).to_wire().to_string();
        assert!(!shown.contains("secret-path"), "leaked: {shown}");
    }

    #[test]
    fn contract_violation_is_internal() {
        assert!(matches!(
            DomainError::contract("m", "x").to_wire(),
            PromptScannerError::Internal(_)
        ));
    }
}
