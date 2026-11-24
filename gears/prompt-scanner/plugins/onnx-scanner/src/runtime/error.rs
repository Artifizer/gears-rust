use thiserror::Error;

/// Transport-agnostic error type for [`super::ModelRuntime`].
#[derive(Debug, Clone, Error)]
pub enum RuntimeError {
    /// No model with this id is configured — a caller-side mistake, usually a
    /// typo. Distinct from [`Self::ModelUnavailable`], which is an outage.
    #[error("unknown model id: {model_id}")]
    UnknownModel { model_id: String },

    /// The model is configured but not usable: it failed to load, or lost its
    /// sessions and could not rebuild them.
    ///
    /// Consumers must map this to degraded coverage rather than to an absence
    /// of findings — a scan that could not run is not a clean scan.
    #[error("model {model_id} is unavailable: {reason}")]
    ModelUnavailable {
        model_id: String,
        reason: super::types::ModelFailureReason,
    },

    #[error("tensor contract mismatch for model {model_id}: {message}")]
    ContractMismatch { model_id: String, message: String },

    /// The caller's input was rejected before any work started — a shape that
    /// does not match its data, an unknown tensor name, an oversized payload.
    ///
    /// A caller-side bug. Retrying the same input will fail the same way.
    #[error("invalid input for model {model_id}, tensor {tensor}: {message}")]
    InvalidInput {
        model_id: String,
        tensor: String,
        message: String,
    },

    /// The inference did not finish within the model's configured deadline.
    ///
    /// **The work was abandoned, not cancelled.** ONNX Runtime cannot stop an
    /// inference mid-graph, so the replica stays occupied until the underlying
    /// call finishes on its own. Retrying immediately will therefore compete
    /// with work that is still running.
    #[error("inference timed out for model {model_id} after {elapsed_ms}ms")]
    Timeout { model_id: String, elapsed_ms: u64 },

    /// Too many inferences are already in flight for this model.
    ///
    /// Transient and retryable, unlike [`Self::InvalidInput`]: the same request
    /// may well succeed once load drops.
    #[error("model {model_id} is at capacity ({max_in_flight} in flight)")]
    Overloaded {
        model_id: String,
        /// The configured ceiling that was reached.
        max_in_flight: usize,
    },

    #[error("inference failed for model {model_id}: {message}")]
    InferenceFailed { model_id: String, message: String },

    #[error("tokenization failed for model {model_id}: {message}")]
    TokenizationFailed { model_id: String, message: String },

    #[error("internal error: {0}")]
    Internal(String),
}

impl RuntimeError {
    /// Whether retrying the identical request could plausibly succeed.
    ///
    /// Consumers use this to separate "this call failed" from "this input is
    /// wrong" — the two map to different coverage outcomes upstream. It is a
    /// classification of the *error*, not advice about backoff.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        match self {
            // Load shed and deadlines are functions of load, which changes.
            Self::Timeout { .. } | Self::Overloaded { .. } => true,
            // A model can come back only by restart, and the rest are all
            // caller-side or deterministic.
            Self::UnknownModel { .. }
            | Self::ModelUnavailable { .. }
            | Self::ContractMismatch { .. }
            | Self::InvalidInput { .. }
            | Self::InferenceFailed { .. }
            | Self::TokenizationFailed { .. }
            | Self::Internal(_) => false,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::runtime::ModelFailureReason;

    /// The whole classification, one row per variant: a new variant must be
    /// added here, and a changed answer is a visible diff.
    #[test]
    fn retryability_table() {
        let m = || "m".to_owned();
        let cases = [
            (RuntimeError::UnknownModel { model_id: m() }, false),
            (
                RuntimeError::ModelUnavailable {
                    model_id: m(),
                    reason: ModelFailureReason::SessionBuildFailed,
                },
                false,
            ),
            (
                RuntimeError::ContractMismatch {
                    model_id: m(),
                    message: String::new(),
                },
                false,
            ),
            (
                RuntimeError::InvalidInput {
                    model_id: m(),
                    tensor: "t".to_owned(),
                    message: String::new(),
                },
                false,
            ),
            (
                RuntimeError::Timeout {
                    model_id: m(),
                    elapsed_ms: 1,
                },
                true,
            ),
            (
                RuntimeError::Overloaded {
                    model_id: m(),
                    max_in_flight: 1,
                },
                true,
            ),
            (
                RuntimeError::InferenceFailed {
                    model_id: m(),
                    message: String::new(),
                },
                false,
            ),
            (
                RuntimeError::TokenizationFailed {
                    model_id: m(),
                    message: String::new(),
                },
                false,
            ),
            (RuntimeError::Internal(String::new()), false),
        ];
        for (err, retryable) in cases {
            assert_eq!(err.is_retryable(), retryable, "{err}");
        }
    }

    #[test]
    fn overloaded_reports_the_ceiling() {
        let err = RuntimeError::Overloaded {
            model_id: "m".to_owned(),
            max_in_flight: 7,
        };
        assert_eq!(err.to_string(), "model m is at capacity (7 in flight)");
    }
}
