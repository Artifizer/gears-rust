//! `GLiNER2` prompt construction, ported from `dlp-bench`'s
//! `src/detectors/gliner2/prompt.rs`, adapted to tokenize via
//! `ModelRuntime::encode_fragment` (async, in-process call) instead of a
//! locally-held `tokenizers::Tokenizer`.
//!
//! The encoder input is *not* a plain tokenization of the text. It is a
//! schema prompt followed by the text, with no `[CLS]`/`[SEP]`:
//!
//! ```text
//! (  [P]  entities  (  [E] label0  [E] label1 ... )  )  [SEP_TEXT]  w0 w1 ...
//! ```
//!
//! Each *word* in that sequence is tokenized independently with
//! `add_special_tokens = false` and the sub-token id lists are
//! concatenated. Two asymmetries from the reference that are easy to get
//! backwards:
//!
//! * schema label strings are **not** lowercased;
//! * text words **are** lowercased.
//!
//! `schema_special_indices` is `[position of [P]] + [position of each
//! [E]]`, hence its length is `num_labels + 1` — the graph reads the `[P]`
//! embedding for the count head and the `[E]` embeddings as label
//! representations.
//!
//! # Why the prefix is precomputed
//!
//! Every token position in the schema section depends only on preceding
//! schema tokens, so the prefix ids and `schema_special_indices` are
//! text-independent and are built once at gear init. The graph consumes
//! raw ids plus index tensors and never re-derives the schema, so this is
//! exact, not an approximation.
//!
//! # The word cache and concurrency
//!
//! Text words are tokenized through a backend-wide [`WordIdCache`]. The cache
//! lock is **never held across an `.await`**: [`build_input`] looks up every
//! word under the lock, releases it, tokenizes the distinct misses in one
//! batch call ([`ModelRuntime::encode_fragments`]), then
//! takes the lock again to assemble the input and store the misses. Two
//! concurrent scans may both tokenize the same new word; the result is the
//! same, so the only cost is the duplicate call.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Mutex, PoisonError};

use crate::domain::error::DomainError;
use crate::runtime::ModelRuntime;

use super::words::Word;

/// Special token ids, verified against the shipped `tokenizer.json` at
/// load — see `Gliner2Backend::load`.
pub mod special {
    pub const SEP_TEXT: &str = "[SEP_TEXT]";
    pub const P: &str = "[P]";
    pub const E: &str = "[E]";

    pub const SEP_TEXT_ID: i64 = 250_103;
    pub const P_ID: i64 = 250_104;
    pub const E_ID: i64 = 250_106;
}

/// The precomputed, text-independent part of the encoder input.
#[derive(Debug, Clone)]
pub struct PromptPrefix {
    /// Token ids for `( [P] entities ( [E] L0 ... ) ) [SEP_TEXT]`.
    pub ids: Vec<i64>,
    /// `[pos([P])] + [pos([E]_i) for each label]`; length `num_labels + 1`.
    pub schema_special_indices: Vec<i64>,
    pub num_labels: usize,
}

impl PromptPrefix {
    pub fn len(&self) -> usize {
        self.ids.len()
    }
}

/// Caches sub-token ids per lowercased word. Unbounded reset (not LRU) once
/// `cap` is hit — correctness does not depend on eviction policy because
/// encoding is pure, so this is just a bounded-memory cache.
#[derive(Debug, Default)]
pub struct WordIdCache {
    map: HashMap<Box<str>, Vec<i64>>,
    cap: usize,
}

impl WordIdCache {
    pub fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            cap,
        }
    }

    fn get(&self, word: &str) -> Option<&[i64]> {
        self.map.get(word).map(Vec::as_slice)
    }

    fn insert(&mut self, word: Box<str>, ids: Vec<i64>) {
        if self.map.len() >= self.cap {
            self.map.clear(); // cheap bounded reset; hit rate recovers immediately
        }
        self.map.insert(word, ids);
    }
}

/// Tokenize one word to ids, with no special tokens added — the exact
/// analogue of the reference's `convert_tokens_to_ids(tokenizer.tokenize(word))`.
pub async fn encode_fragment(
    onnx_client: &dyn ModelRuntime,
    model_id: &str,
    word: &str,
) -> Result<Vec<i64>, DomainError> {
    onnx_client
        .encode_fragment(model_id, word)
        .await
        .map_err(|e| DomainError::runtime(model_id, e))
}

/// Current length of `ids` as an `i64` position.
fn index_of(model_id: &str, ids: &[i64]) -> Result<i64, DomainError> {
    i64::try_from(ids.len())
        .map_err(|_| DomainError::contract(model_id, "encoder input length overflows i64"))
}

async fn push_word(
    ids: &mut Vec<i64>,
    onnx_client: &dyn ModelRuntime,
    model_id: &str,
    word: &str,
) -> Result<(), DomainError> {
    ids.extend(encode_fragment(onnx_client, model_id, word).await?);
    Ok(())
}

/// Build the schema prefix for a fixed label set.
///
/// # Errors
///
/// [`DomainError::InvalidConfig`] for an empty label set; a runtime error if
/// tokenization fails.
pub async fn build_prefix(
    onnx_client: &dyn ModelRuntime,
    model_id: &str,
    labels: &[String],
) -> Result<PromptPrefix, DomainError> {
    if labels.is_empty() {
        return Err(DomainError::InvalidConfig(
            "`GLiNER2` schema must have at least one label".to_owned(),
        ));
    }

    // Word sequence, mirroring `_transform_schema` +
    // `_format_input_with_mapping`. Note the single schema means the
    // `[SEP_STRUCT]` the reference appends is immediately popped, so it
    // never appears.
    let mut ids: Vec<i64> = Vec::with_capacity(labels.len() * 4 + 8);
    let mut schema_special_indices: Vec<i64> = Vec::with_capacity(labels.len() + 1);

    push_word(&mut ids, onnx_client, model_id, "(").await?;
    // `[P]` marks the parent; its embedding feeds the count head.
    schema_special_indices.push(index_of(model_id, &ids)?);
    push_word(&mut ids, onnx_client, model_id, special::P).await?;
    push_word(&mut ids, onnx_client, model_id, "entities").await?;
    push_word(&mut ids, onnx_client, model_id, "(").await?;
    for label in labels {
        // `[E]` marks a child field; its embedding is that label's query vector.
        schema_special_indices.push(index_of(model_id, &ids)?);
        push_word(&mut ids, onnx_client, model_id, special::E).await?;
        // Label strings keep their case — the reference does not lowercase them.
        push_word(&mut ids, onnx_client, model_id, label).await?;
    }
    push_word(&mut ids, onnx_client, model_id, ")").await?;
    push_word(&mut ids, onnx_client, model_id, ")").await?;
    push_word(&mut ids, onnx_client, model_id, special::SEP_TEXT).await?;

    debug_assert_eq!(schema_special_indices.len(), labels.len() + 1);

    Ok(PromptPrefix {
        ids,
        schema_special_indices,
        num_labels: labels.len(),
    })
}

/// Per-document encoder input.
#[derive(Debug, Default)]
pub struct EncodedInput {
    pub input_ids: Vec<i64>,
    pub attention_mask: Vec<i64>,
    /// Absolute position of each text word's first sub-token.
    pub text_word_indices: Vec<i64>,
}

impl EncodedInput {
    pub fn seq_len(&self) -> usize {
        self.input_ids.len()
    }
}

/// Where a word's ids come from once lookups are done.
enum Piece {
    /// Copied out of the cache into the scratch buffer, at this range.
    Hit(Range<usize>),
    /// The n-th distinct word that missed the cache.
    Miss(usize),
}

fn lock(cache: &Mutex<WordIdCache>) -> std::sync::MutexGuard<'_, WordIdCache> {
    // A poisoned cache is still a valid cache: entries are written whole.
    cache.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Append the text words to the precomputed prefix.
///
/// # Errors
///
/// A runtime error if tokenizing a word fails; a contract error if the
/// assembled input is empty or its length overflows `i64`.
pub async fn build_input(
    prefix: &PromptPrefix,
    words: &[Word],
    onnx_client: &dyn ModelRuntime,
    model_id: &str,
    cache: &Mutex<WordIdCache>,
) -> Result<EncodedInput, DomainError> {
    // Phase 1, under the lock: copy every hit out, list the distinct misses.
    let mut scratch: Vec<i64> = Vec::new();
    let mut pieces: Vec<Piece> = Vec::with_capacity(words.len());
    let mut misses: Vec<&str> = Vec::new();
    {
        let mut miss_index: HashMap<&str, usize> = HashMap::new();
        let guard = lock(cache);
        for w in words {
            let word: &str = &w.lower;
            if let Some(ids) = guard.get(word) {
                let start = scratch.len();
                scratch.extend_from_slice(ids);
                pieces.push(Piece::Hit(start..scratch.len()));
            } else {
                let next = misses.len();
                let idx = *miss_index.entry(word).or_insert_with(|| {
                    misses.push(word);
                    next
                });
                pieces.push(Piece::Miss(idx));
            }
        }
    }

    // Phase 2, no lock held: tokenize every distinct miss in one batch call
    // (one blocking-pool hop instead of one per word).
    let miss_ids: Vec<Vec<i64>> = if misses.is_empty() {
        Vec::new()
    } else {
        let ids = onnx_client
            .encode_fragments(model_id, &misses)
            .await
            .map_err(|e| DomainError::runtime(model_id, e))?;
        if ids.len() != misses.len() {
            return Err(DomainError::contract(
                model_id,
                "batch tokenization returned a different number of fragments than requested",
            ));
        }
        ids
    };

    // Phase 3: assemble.
    let mut out = EncodedInput {
        input_ids: Vec::with_capacity(prefix.ids.len() + scratch.len() + words.len()),
        attention_mask: Vec::new(),
        text_word_indices: Vec::with_capacity(words.len()),
    };
    out.input_ids.extend_from_slice(&prefix.ids);
    for piece in &pieces {
        // Recorded *before* extending, which reproduces the reference's
        // behaviour for a word that tokenizes to nothing: it inherits the
        // next word's first-subtoken position.
        out.text_word_indices
            .push(index_of(model_id, &out.input_ids)?);
        let ids: &[i64] = match piece {
            Piece::Hit(range) => &scratch[range.clone()],
            Piece::Miss(i) => &miss_ids[*i],
        };
        out.input_ids.extend_from_slice(ids);
    }

    // Store the misses, moving rather than cloning their ids.
    if !misses.is_empty() {
        let mut guard = lock(cache);
        for (word, ids) in misses.into_iter().zip(miss_ids) {
            guard.insert(word.into(), ids);
        }
    }

    if out.input_ids.is_empty() {
        return Err(DomainError::contract(
            model_id,
            "empty gliner2 encoder input",
        ));
    }

    // The reference has a latent out-of-bounds here: a *trailing*
    // zero-subtoken word records an index equal to `seq_len`, and the
    // gather then reads past the end. Clamp rather than reproduce a crash.
    let last = index_of(model_id, &out.input_ids)? - 1;
    for v in &mut out.text_word_indices {
        if *v > last {
            *v = last;
        }
    }

    out.attention_mask = vec![1; out.input_ids.len()];
    Ok(out)
}

/// Load-time sanity check: a tokenizer whose special-token ids differ is
/// not this model's tokenizer, and every index we compute would be
/// silently wrong.
///
/// # Errors
///
/// [`DomainError::TokenizerMismatch`] when a special token does not encode to
/// its expected single id.
pub async fn assert_special_tokens(
    onnx_client: &dyn ModelRuntime,
    model_id: &str,
) -> Result<(), DomainError> {
    for (name, want) in [
        (special::P, special::P_ID),
        (special::E, special::E_ID),
        (special::SEP_TEXT, special::SEP_TEXT_ID),
    ] {
        let ids = encode_fragment(onnx_client, model_id, name).await?;
        if ids != [want] {
            return Err(DomainError::TokenizerMismatch {
                model_id: model_id.to_owned(),
                detail: format!(
                    "special token {name} encodes to {ids:?}, expected [{want}]; \
                     this is not the tokenizer gliner2 was built against"
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::FakeRuntime;
    use super::super::words;
    use super::*;

    fn labels(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[tokio::test]
    async fn build_prefix_records_p_and_each_e_position() {
        let rt = FakeRuntime::default();
        let prefix = build_prefix(&rt, "m", &labels(&["person", "email"]))
            .await
            .expect("builds");

        // FakeRuntime: every ordinary word is one token, special tokens map to
        // their real ids. Sequence: ( [P] entities ( [E] person [E] email ) ) [SEP_TEXT]
        assert_eq!(prefix.len(), 11);
        assert_eq!(prefix.num_labels, 2);
        assert_eq!(prefix.schema_special_indices, vec![1, 4, 6]);
        assert_eq!(prefix.ids[1], special::P_ID);
        assert_eq!(prefix.ids[4], special::E_ID);
        assert_eq!(prefix.ids[6], special::E_ID);
        assert_eq!(prefix.ids[10], special::SEP_TEXT_ID);
    }

    #[tokio::test]
    async fn build_prefix_rejects_an_empty_schema() {
        let rt = FakeRuntime::default();
        assert!(matches!(
            build_prefix(&rt, "m", &[]).await,
            Err(DomainError::InvalidConfig(_))
        ));
    }

    #[tokio::test]
    async fn word_indices_point_at_each_words_first_subtoken() {
        let rt = FakeRuntime::default();
        let prefix = build_prefix(&rt, "m", &labels(&["person"]))
            .await
            .expect("prefix");
        let cache = Mutex::new(WordIdCache::new(100));
        // "longword" is two sub-tokens in the fake.
        let prepared = words::prepare("a longword b");
        let enc = build_input(&prefix, &prepared.words, &rt, "m", &cache)
            .await
            .expect("input");

        let p = i64::try_from(prefix.len()).expect("small");
        // a, longword (2), b, '.'
        assert_eq!(enc.text_word_indices, vec![p, p + 1, p + 3, p + 4]);
        assert_eq!(enc.seq_len(), prefix.len() + 5);
        assert_eq!(enc.attention_mask.len(), enc.seq_len());
    }

    #[tokio::test]
    async fn a_trailing_zero_subtoken_word_is_clamped_into_range() {
        let rt = FakeRuntime::default();
        let prefix = build_prefix(&rt, "m", &labels(&["person"]))
            .await
            .expect("prefix");
        let cache = Mutex::new(WordIdCache::new(100));
        // "a" then the fake's zero-subtoken word last, as words directly
        // so no synthetic '.' follows it.
        let ws = vec![
            Word {
                lower: "a".into(),
                start: 0,
                end: 1,
            },
            Word {
                lower: FakeRuntime::EMPTY_WORD.into(),
                start: 2,
                end: 3,
            },
        ];
        let enc = build_input(&prefix, &ws, &rt, "m", &cache)
            .await
            .expect("input");
        let last = i64::try_from(enc.seq_len() - 1).expect("small");
        assert_eq!(
            enc.text_word_indices,
            vec![last, last],
            "clamped, not seq_len"
        );
    }

    #[tokio::test]
    async fn repeated_words_are_tokenized_once_and_cached() {
        let rt = FakeRuntime::default();
        let prefix = build_prefix(&rt, "m", &labels(&["person"]))
            .await
            .expect("prefix");
        let before = rt.fragment_calls();
        let cache = Mutex::new(WordIdCache::new(100));
        let prepared = words::prepare("x x x y");
        build_input(&prefix, &prepared.words, &rt, "m", &cache)
            .await
            .expect("input");
        // x, y and '.' — once each.
        assert_eq!(rt.fragment_calls() - before, 3);

        build_input(&prefix, &prepared.words, &rt, "m", &cache)
            .await
            .expect("input");
        assert_eq!(
            rt.fragment_calls() - before,
            3,
            "second pass is all cache hits"
        );
    }

    #[tokio::test]
    async fn special_token_check_rejects_a_foreign_tokenizer() {
        let rt = FakeRuntime {
            wrong_special_tokens: true,
            ..FakeRuntime::default()
        };
        assert!(matches!(
            assert_special_tokens(&rt, "m").await,
            Err(DomainError::TokenizerMismatch { .. })
        ));
        assert_special_tokens(&FakeRuntime::default(), "m")
            .await
            .expect("the right tokenizer passes");
    }
}
