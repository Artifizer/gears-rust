//! BIO-tag decoding for a plain token-classification model (`minilm-l6-pii`
//! and similar).
//! No prompt encoding, no span-probability decoding (that's
//! `domain::gliner2`) — just per-token argmax over a fixed label set, then
//! merging consecutive `B-X`/`I-X` tokens into entity spans.

use crate::domain::entity::Entity;

/// A single token's predicted label and the model's confidence in it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenPrediction<'a> {
    pub label: &'a str,
    pub confidence: f32,
}

/// Why logits could not be decoded. Always a model/config contract problem,
/// never "no entities".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LogitsError {
    #[error("no labels configured")]
    NoLabels,
    #[error("expected {expected} logits ({seq_len} tokens x {num_labels} labels), got {actual}")]
    Shape {
        seq_len: usize,
        num_labels: usize,
        expected: usize,
        actual: usize,
    },
    #[error("token {token} has a NaN logit")]
    NaN { token: usize },
}

/// Index and softmax probability of the row's largest logit.
///
/// Ties keep the **first** maximum, as `torch.argmax` does. The probability is
/// computed directly (`1 / sum(exp(v - max))`) rather than materializing the
/// whole softmax row.
fn best_of(row: &[f32]) -> Option<(usize, f32)> {
    let mut best_idx = 0;
    let mut max = *row.first()?;
    for (i, &v) in row.iter().enumerate().skip(1) {
        if v > max {
            max = v;
            best_idx = i;
        }
    }
    let sum: f32 = row.iter().map(|&v| (v - max).exp()).sum();
    let confidence = if sum.is_finite() && sum > 0.0 {
        1.0 / sum
    } else {
        0.0
    };
    Some((best_idx, confidence))
}

/// Per-token argmax over `logits` (flattened `[seq_len, labels.len()]`,
/// row-major), mapped to `labels`.
///
/// # Errors
///
/// [`LogitsError`] when there are no labels, when `logits.len()` is not
/// exactly `seq_len * labels.len()` (a model whose label count disagrees with
/// the configured labels would otherwise be silently misclassified), or when
/// a logit is NaN.
pub fn argmax_predictions<'a>(
    logits: &[f32],
    seq_len: usize,
    labels: &'a [String],
) -> Result<Vec<TokenPrediction<'a>>, LogitsError> {
    let num_labels = labels.len();
    if num_labels == 0 {
        return Err(LogitsError::NoLabels);
    }
    let expected = seq_len.saturating_mul(num_labels);
    if logits.len() != expected {
        return Err(LogitsError::Shape {
            seq_len,
            num_labels,
            expected,
            actual: logits.len(),
        });
    }
    logits
        .chunks_exact(num_labels)
        .enumerate()
        .map(|(token, row)| {
            if row.iter().any(|v| v.is_nan()) {
                return Err(LogitsError::NaN { token });
            }
            // `row` is never empty: `num_labels > 0`.
            let (best_idx, confidence) = best_of(row).ok_or(LogitsError::NoLabels)?;
            Ok(TokenPrediction {
                label: labels[best_idx].as_str(),
                confidence,
            })
        })
        .collect()
}

/// A BIO label, split into its tag and category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bio<'a> {
    Begin(&'a str),
    Inside(&'a str),
    Outside,
}

impl<'a> Bio<'a> {
    fn parse(label: &'a str) -> Self {
        if let Some(rest) = label.strip_prefix("B-") {
            Self::Begin(rest)
        } else if let Some(rest) = label.strip_prefix("I-") {
            Self::Inside(rest)
        } else {
            Self::Outside
        }
    }
}

struct OpenEntity<'a> {
    category: &'a str,
    start: u32,
    end: u32,
    confidence_sum: f32,
    tokens: u32,
}

impl<'a> OpenEntity<'a> {
    fn new(category: &'a str, start: u32, end: u32, confidence: f32) -> Self {
        Self {
            category,
            start,
            end,
            confidence_sum: confidence,
            tokens: 1,
        }
    }

    fn extend(&mut self, end: u32, confidence: f32) {
        self.end = end;
        self.confidence_sum += confidence;
        self.tokens += 1;
    }

    /// The entity's confidence is the mean of its tokens' confidences.
    fn finish(self) -> Entity {
        #[expect(
            clippy::cast_precision_loss,
            reason = "token counts per entity are bounded by the model's sequence length, far below 2^24"
        )]
        let mean = self.confidence_sum / self.tokens as f32;
        Entity {
            category: self.category.to_owned(),
            start: self.start,
            end: self.end,
            confidence: mean,
        }
    }
}

/// Merges per-token BIO predictions into entity spans, using `offsets`
/// (byte spans into the original text, per token) and
/// `special_tokens_mask` (`[CLS]`/`[SEP]`/padding tokens, which always
/// close any open entity and are otherwise ignored).
///
/// A lenient merge: an `I-X` immediately after an unrelated `B-Y`/`I-Y` (or
/// with no preceding `B-`/`I-` at all) starts a new entity rather than being
/// dropped — real model output sometimes skips a `B-` tag on the first
/// token of a span.
#[must_use]
pub fn merge_bio(
    predictions: &[TokenPrediction<'_>],
    offsets: &[(u32, u32)],
    special_tokens_mask: &[bool],
) -> Vec<Entity> {
    let mut entities = Vec::new();
    let mut current: Option<OpenEntity<'_>> = None;

    for (i, pred) in predictions.iter().enumerate() {
        if special_tokens_mask.get(i).copied().unwrap_or(false) {
            entities.extend(current.take().map(OpenEntity::finish));
            continue;
        }
        let Some(&(start, end)) = offsets.get(i) else {
            continue;
        };

        match Bio::parse(pred.label) {
            Bio::Outside => entities.extend(current.take().map(OpenEntity::finish)),
            Bio::Begin(category) => {
                entities.extend(current.take().map(OpenEntity::finish));
                current = Some(OpenEntity::new(category, start, end, pred.confidence));
            }
            Bio::Inside(category) => match &mut current {
                Some(open) if open.category == category => open.extend(end, pred.confidence),
                _ => {
                    entities.extend(current.take().map(OpenEntity::finish));
                    current = Some(OpenEntity::new(category, start, end, pred.confidence));
                }
            },
        }
    }

    entities.extend(current.take().map(OpenEntity::finish));
    entities
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels() -> Vec<String> {
        ["O", "B-EMAIL", "I-EMAIL", "B-PERSON", "I-PERSON"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    }

    #[test]
    fn argmax_predictions_picks_highest_logit() {
        let labels = labels();
        // One token, logits favor B-EMAIL (index 1).
        let logits = [0.1, 5.0, 0.2, 0.1, 0.1];
        let preds = argmax_predictions(&logits, 1, &labels).expect("decodes");
        assert_eq!(preds.len(), 1);
        assert_eq!(preds[0].label, "B-EMAIL");
        assert!(preds[0].confidence > 0.9);
    }

    #[test]
    fn argmax_predictions_rejects_empty_labels() {
        assert_eq!(
            argmax_predictions(&[1.0, 2.0], 1, &[]),
            Err(LogitsError::NoLabels)
        );
    }

    #[test]
    fn argmax_predictions_rejects_a_label_count_mismatch() {
        let labels = labels();
        // Two tokens' worth of logits for a 4-label model, decoded with 5.
        let err = argmax_predictions(&[0.0; 8], 2, &labels).expect_err("shape mismatch");
        assert!(matches!(
            err,
            LogitsError::Shape {
                expected: 10,
                actual: 8,
                ..
            }
        ));
        // Too many logits is just as wrong as too few.
        assert!(argmax_predictions(&[0.0; 15], 2, &labels).is_err());
    }

    #[test]
    fn argmax_predictions_keeps_the_first_maximum_on_ties() {
        let labels = labels();
        let preds = argmax_predictions(&[1.0, 3.0, 3.0, 3.0, 0.0], 1, &labels).expect("decodes");
        assert_eq!(preds[0].label, "B-EMAIL", "first of the tied maxima");
    }

    #[test]
    fn argmax_predictions_rejects_nan() {
        let labels = labels();
        assert_eq!(
            argmax_predictions(
                &[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, f32::NAN, 0.0, 0.0, 0.0],
                2,
                &labels
            ),
            Err(LogitsError::NaN { token: 1 })
        );
    }

    #[test]
    fn argmax_confidence_is_the_softmax_probability() {
        let labels: Vec<String> = vec!["O".to_owned(), "B-X".to_owned()];
        // Equal logits: probability 0.5 each.
        let preds = argmax_predictions(&[2.0, 2.0], 1, &labels).expect("decodes");
        assert!((preds[0].confidence - 0.5).abs() < 1e-6);
    }

    fn pred(label: &str, confidence: f32) -> TokenPrediction<'_> {
        TokenPrediction { label, confidence }
    }

    #[test]
    fn merge_bio_merges_consecutive_i_tags_into_one_entity() {
        let preds = vec![
            pred("O", 0.9),
            pred("B-EMAIL", 0.8),
            pred("I-EMAIL", 0.85),
            pred("O", 0.9),
        ];
        let offsets = vec![(0, 5), (6, 12), (12, 24), (25, 26)];
        let special = vec![false, false, false, false];

        let entities = merge_bio(&preds, &offsets, &special);
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].category, "EMAIL");
        assert_eq!(entities[0].start, 6);
        assert_eq!(entities[0].end, 24);
    }

    #[test]
    fn merged_confidence_is_the_mean_of_its_tokens() {
        let preds = vec![
            pred("B-EMAIL", 0.6),
            pred("I-EMAIL", 0.8),
            pred("I-EMAIL", 1.0),
        ];
        let offsets = vec![(0, 2), (2, 4), (4, 6)];
        let entities = merge_bio(&preds, &offsets, &[false; 3]);
        assert_eq!(entities.len(), 1);
        assert!((entities[0].confidence - 0.8).abs() < 1e-6);
    }

    #[test]
    fn merge_bio_splits_entities_of_different_categories() {
        let preds = vec![pred("B-EMAIL", 0.9), pred("B-PERSON", 0.9)];
        let offsets = vec![(0, 5), (6, 10)];
        let special = vec![false, false];

        let entities = merge_bio(&preds, &offsets, &special);
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].category, "EMAIL");
        assert_eq!(entities[1].category, "PERSON");
    }

    #[test]
    fn an_i_tag_of_another_category_starts_a_new_entity() {
        let preds = vec![pred("B-EMAIL", 0.9), pred("I-PERSON", 0.7)];
        let entities = merge_bio(&preds, &[(0, 5), (5, 9)], &[false, false]);
        assert_eq!(entities.len(), 2);
        assert_eq!((entities[1].start, entities[1].end), (5, 9));
        assert_eq!(entities[1].category, "PERSON");
    }

    #[test]
    fn merge_bio_closes_entity_at_special_token() {
        let preds = vec![pred("B-EMAIL", 0.9), pred("O", 0.5)];
        let offsets = vec![(0, 5), (0, 0)];
        let special = vec![false, true];

        let entities = merge_bio(&preds, &offsets, &special);
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].end, 5);
    }

    #[test]
    fn merge_bio_treats_lone_i_tag_as_a_new_entity() {
        let preds = vec![pred("I-EMAIL", 0.7)];
        let offsets = vec![(0, 5)];
        let special = vec![false];

        let entities = merge_bio(&preds, &offsets, &special);
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].category, "EMAIL");
    }

    #[test]
    fn edge_cases_yield_no_entities_and_do_not_panic() {
        assert!(merge_bio(&[], &[], &[]).is_empty());
        // All outside.
        assert!(
            merge_bio(
                &[pred("O", 0.9), pred("O", 0.9)],
                &[(0, 1), (2, 3)],
                &[false; 2]
            )
            .is_empty()
        );
        // Predictions without offsets are skipped rather than indexed.
        assert!(merge_bio(&[pred("B-EMAIL", 0.9)], &[], &[]).is_empty());
        // A label with no BIO prefix counts as outside.
        assert!(merge_bio(&[pred("EMAIL", 0.9)], &[(0, 5)], &[false]).is_empty());
    }
}
