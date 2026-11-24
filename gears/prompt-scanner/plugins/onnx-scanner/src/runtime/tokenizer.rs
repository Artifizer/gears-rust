//! The ONLY file in this gear that imports `tokenizers`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokenizers::Tokenizer;

use super::config::Sha256Digest;
use super::metrics_port::{InferenceOutcome, NoopMetrics, RuntimeMetricsPort};
use super::{RuntimeError, TokenEncoding};

/// Why a `tokenizer.json` was not loaded. Typed, so the operator log keeps the
/// I/O or parse error rather than a flattened string.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TokenizerLoadError {
    #[error("reading tokenizer {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("tokenizer {path} hashes to {actual}, expected {expected}")]
    ChecksumMismatch {
        path: PathBuf,
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
    #[error("tokenizer {path} could not be parsed: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: tokenizers::Error,
    },
}

/// Tokenization is lighter than inference but not free — large inputs and
/// `onig` regex work both cost real CPU — so it runs on the blocking pool for
/// the same reason inference does. `Tokenizer::encode` takes `&self`, so
/// unlike a session no exclusion is needed: an `Arc` clone moves into the
/// blocking task and any number of encodes proceed concurrently.
pub struct LoadedTokenizer {
    tokenizer: Arc<Tokenizer>,
    metrics: Arc<dyn RuntimeMetricsPort>,
}

impl LoadedTokenizer {
    /// Loads a `tokenizer.json` from disk, unpinned.
    ///
    /// Blocking file I/O: call it from a blocking context (the host does, via
    /// `spawn_blocking`).
    ///
    /// # Errors
    ///
    /// See [`TokenizerLoadError`].
    pub fn load(path: &Path) -> Result<Self, TokenizerLoadError> {
        Self::load_verified(path, None)
    }

    /// Loads a `tokenizer.json`, checking it against `expected` when given.
    ///
    /// The bytes are read once, hashed, and parsed from that same buffer, so
    /// the file that was verified is the file that is used. Blocking.
    ///
    /// # Errors
    ///
    /// See [`TokenizerLoadError`].
    pub fn load_verified(
        path: &Path,
        expected: Option<Sha256Digest>,
    ) -> Result<Self, TokenizerLoadError> {
        let bytes = std::fs::read(path).map_err(|source| TokenizerLoadError::Io {
            path: path.to_owned(),
            source,
        })?;
        if let Some(expected) = expected {
            let actual = super::session::sha256_of(&bytes);
            if actual != expected {
                return Err(TokenizerLoadError::ChecksumMismatch {
                    path: path.to_owned(),
                    expected,
                    actual,
                });
            }
        }
        let tokenizer =
            Tokenizer::from_bytes(&bytes).map_err(|source| TokenizerLoadError::Parse {
                path: path.to_owned(),
                source,
            })?;
        Ok(Self {
            tokenizer: Arc::new(tokenizer),
            metrics: Arc::new(NoopMetrics),
        })
    }

    /// Attaches the metrics port. Called once at gear init.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn RuntimeMetricsPort>) -> Self {
        self.metrics = metrics;
        self
    }

    /// Runs `f` on the blocking pool with a handle to the tokenizer.
    ///
    /// Cancel-safe: dropping the returned future discards the result and
    /// leaves nothing to clean up, since the tokenizer is shared rather than
    /// taken.
    async fn with_tokenizer<T, F>(&self, model_id: &str, f: F) -> Result<T, RuntimeError>
    where
        F: FnOnce(&Tokenizer) -> Result<T, RuntimeError> + Send + 'static,
        T: Send + 'static,
    {
        let tokenizer = Arc::clone(&self.tokenizer);
        let owned_id = model_id.to_owned();
        let started = Instant::now();

        let result = tokio::task::spawn_blocking(move || f(&tokenizer))
            .await
            .map_err(|join_err| {
                RuntimeError::Internal(format!(
                    "tokenization task for model {owned_id} did not complete: {join_err}"
                ))
            })
            .and_then(|inner| inner);

        let outcome = match &result {
            Ok(_) => InferenceOutcome::Ok,
            Err(e) => e.into(),
        };
        self.metrics
            .record_tokenize(model_id, outcome, started.elapsed());
        result
    }

    /// Encodes `text`, returning ids/attention-mask/special-tokens-mask plus
    /// offsets into the original `text`.
    ///
    /// **Offsets are byte offsets, and this is verified**, not assumed:
    /// `tests/tokenizer_offset_tests.rs` slices the original text by every
    /// returned offset pair and asserts it reproduces that token, across
    /// Cyrillic, CJK and combining sequences. Measured: `привет` is 6
    /// characters and 12 bytes, and returns `(0, 12)`.
    ///
    /// This mattered because the wrong implementation fails quietly. A
    /// character offset used as a byte offset still slices — into a shorter,
    /// wrong span — so every consumer mapping a span back to source text would
    /// have been silently wrong for non-ASCII input. Note that this is *not*
    /// the same data path as `GLiNER2`'s own Python-side output layer, which
    /// dlp-bench separately found to be character-offset; do not generalize
    /// this result to that one.
    /// # Errors
    ///
    /// Returns [`RuntimeError::TokenizationFailed`] if the tokenizer
    /// rejects the input, or [`RuntimeError::Internal`] if the blocking
    /// task does not complete.
    #[tracing::instrument(
        name = "onnx_scanner.runtime.tokenizer.encode",
        skip_all,
        fields(model_id = %model_id, text_bytes = text.len())
    )]
    pub async fn encode(
        &self,
        model_id: &str,
        text: String,
    ) -> Result<TokenEncoding, RuntimeError> {
        // `skip_all` plus an explicit byte count: the length is useful, the
        // text is confidential and attacker-controlled.
        let owned_id = model_id.to_owned();
        self.with_tokenizer(model_id, move |tokenizer| {
            encode_to_token_encoding(tokenizer, &owned_id, &text)
        })
        .await
    }

    /// Tokenize `fragment` to raw sub-token ids with `add_special_tokens =
    /// false` — see `ModelRuntime::encode_fragment`'s docs for why this
    /// is a distinct primitive from [`Self::encode`].
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::TokenizationFailed`] if the tokenizer
    /// rejects the input, or [`RuntimeError::Internal`] if the blocking
    /// task does not complete.
    #[tracing::instrument(
        name = "onnx_scanner.runtime.tokenizer.encode_fragment",
        skip_all,
        fields(model_id = %model_id, text_bytes = fragment.len())
    )]
    pub async fn encode_fragment(
        &self,
        model_id: &str,
        fragment: String,
    ) -> Result<Vec<i64>, RuntimeError> {
        let owned_id = model_id.to_owned();
        self.with_tokenizer(model_id, move |tokenizer| {
            let encoding = tokenizer.encode(fragment, false).map_err(|e| {
                RuntimeError::TokenizationFailed {
                    model_id: owned_id,
                    message: e.to_string(),
                }
            })?;
            Ok(encoding.get_ids().iter().map(|&id| i64::from(id)).collect())
        })
        .await
    }

    /// [`Self::encode_fragment`] for many fragments in one blocking task.
    ///
    /// # Errors
    ///
    /// As [`Self::encode_fragment`]; the first failure fails the batch.
    #[tracing::instrument(
        name = "onnx_scanner.runtime.tokenizer.encode_fragments",
        skip_all,
        fields(model_id = %model_id, fragments = fragments.len())
    )]
    pub async fn encode_fragments(
        &self,
        model_id: &str,
        fragments: Vec<String>,
    ) -> Result<Vec<Vec<i64>>, RuntimeError> {
        let owned_id = model_id.to_owned();
        self.with_tokenizer(model_id, move |tokenizer| {
            let encodings = tokenizer.encode_batch(fragments, false).map_err(|e| {
                RuntimeError::TokenizationFailed {
                    model_id: owned_id,
                    message: e.to_string(),
                }
            })?;
            Ok(encodings
                .iter()
                .map(|encoding| encoding.get_ids().iter().map(|&id| i64::from(id)).collect())
                .collect())
        })
        .await
    }
}

fn encode_to_token_encoding(
    tokenizer: &Tokenizer,
    model_id: &str,
    text: &str,
) -> Result<TokenEncoding, RuntimeError> {
    let encoding = tokenizer
        .encode(text, true)
        .map_err(|e| RuntimeError::TokenizationFailed {
            model_id: model_id.to_owned(),
            message: e.to_string(),
        })?;

    let ids: Vec<i64> = encoding.get_ids().iter().map(|&id| i64::from(id)).collect();
    let attention_mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&m| i64::from(m))
        .collect();
    let special_tokens_mask: Vec<bool> = encoding
        .get_special_tokens_mask()
        .iter()
        .map(|&m| m == 1)
        .collect();
    // `TokenEncoding` carries offsets as u32. The gear caps text length
    // well below u32::MAX, but that cap is operator-set, so the conversion
    // is checked rather than assumed.
    let offsets: Vec<(u32, u32)> = encoding
        .get_offsets()
        .iter()
        .map(|&(start, end)| {
            let to_u32 = |v: usize| {
                u32::try_from(v).map_err(|_| RuntimeError::TokenizationFailed {
                    model_id: model_id.to_owned(),
                    message: format!("byte offset {v} exceeds the u32 offset range"),
                })
            };
            Ok((to_u32(start)?, to_u32(end)?))
        })
        .collect::<Result<_, RuntimeError>>()?;

    Ok(TokenEncoding {
        ids,
        attention_mask,
        offsets,
        special_tokens_mask,
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tokenizer.json")
    }

    fn digest_of_fixture() -> Sha256Digest {
        super::super::session::sha256_of(&std::fs::read(fixture()).expect("fixture readable"))
    }

    #[test]
    fn a_pinned_tokenizer_loads_when_its_digest_matches() {
        assert!(LoadedTokenizer::load_verified(&fixture(), Some(digest_of_fixture())).is_ok());
    }

    #[test]
    fn a_pinned_tokenizer_with_the_wrong_digest_is_refused() {
        let wrong = Sha256Digest::from_bytes([7; 32]);
        let Err(err) = LoadedTokenizer::load_verified(&fixture(), Some(wrong)) else {
            panic!("a wrong digest must not load");
        };
        assert!(
            matches!(err, TokenizerLoadError::ChecksumMismatch { expected, .. } if expected == wrong),
            "{err}"
        );
    }

    #[test]
    fn a_missing_file_is_an_io_error_with_its_source() {
        let Err(err) = LoadedTokenizer::load(&fixture().with_file_name("absent.json")) else {
            panic!("a missing file must not load");
        };
        assert!(matches!(err, TokenizerLoadError::Io { .. }), "{err}");
        assert!(
            std::error::Error::source(&err).is_some(),
            "keeps the io::Error"
        );
    }

    #[test]
    fn a_file_that_is_not_a_tokenizer_is_a_parse_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tokenizer.json");
        std::fs::write(&path, b"{not json").expect("writes");
        let Err(err) = LoadedTokenizer::load(&path) else {
            panic!("garbage must not load");
        };
        assert!(matches!(err, TokenizerLoadError::Parse { .. }), "{err}");
    }
}
