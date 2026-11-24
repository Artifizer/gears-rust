//! Scoring PII detections against gold spans.
//!
//! Deliberately independent of any model or backend: it takes gold spans and
//! predicted [`Entity`]s and produces counts. That keeps the part with the
//! subtle rules — span matching, taxonomy mapping, what counts as a miss —
//! unit-testable without a model.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::domain::entity::Entity;

/// A gold-annotated entity, in byte offsets into the document text — the same
/// coordinate space [`Entity`] uses, so nothing has to be converted at
/// comparison time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoldSpan {
    /// Canonical category. Model categories are mapped into this space before
    /// comparison; see [`TaxonomyMap`].
    pub category: String,
    pub start: u32,
    pub end: u32,
}

/// One scored document.
#[derive(Debug, Clone)]
pub struct Document {
    pub id: String,
    pub text: String,
    pub lang: Option<String>,
    /// Empty means *no PII present*, which is a meaningful assertion — that is
    /// what makes a document a negative control rather than merely unlabelled.
    /// A corpus of unlabelled text must not be fed in here.
    pub spans: Vec<GoldSpan>,
}

/// How closely a predicted span must line up with a gold span to count.
///
/// Reported at more than one strictness rather than picking one, because both
/// are defensible for redaction and the choice changes the number materially:
/// covering most of an entity still leaks the rest, but a boundary off by one
/// character is not the same failure as missing it entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    /// Byte offsets identical.
    Exact,
    /// Any overlap at all, with the category still required to agree.
    Overlap,
}

impl MatchMode {
    fn accepts(self, gold: &GoldSpan, start: u32, end: u32) -> bool {
        match self {
            Self::Exact => gold.start == start && gold.end == end,
            Self::Overlap => start < gold.end && gold.start < end,
        }
    }
}

/// Bytes of separation still treated as one entity when coalescing. One space
/// or one punctuation mark; wide enough to join "Sarah" and "Chen", narrow
/// enough to keep two names in a comma-separated list apart.
pub const ADJACENT_GAP: u32 = 1;

/// Maps a backend's own category names into the canonical taxonomy the gold
/// spans use.
///
/// This is where most measurement error lives, so an unmapped prediction is
/// never silently scored: it is counted apart and surfaced, because a missing
/// mapping entry is a defect in the evaluation rather than in the model.
///
/// An **empty** map means identity: model categories are compared as-is,
/// which is only correct when the corpus was annotated in the model's own
/// space.
pub type TaxonomyMap = HashMap<String, String>;

/// Canonical category for a model category, under [`TaxonomyMap`]'s rules.
fn canonical<'a>(taxonomy: &'a TaxonomyMap, category: &'a str) -> Option<&'a str> {
    if taxonomy.is_empty() {
        Some(category)
    } else {
        taxonomy.get(category).map(String::as_str)
    }
}

/// Corpus counts stay far below f64's exact-integer range, so the conversion
/// is lossless in practice; `f64::from` does not accept `usize`, hence the
/// explicit cast in one place rather than at every call site.
#[expect(
    clippy::cast_precision_loss,
    reason = "corpus counts stay far below 2^53, where the conversion is exact"
)]
fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    numerator as f64 / denominator as f64
}

/// Counts for one category, or summed across all of them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub true_positives: usize,
    pub false_positives: usize,
    pub false_negatives: usize,
}

impl Counts {
    #[must_use]
    pub fn precision(self) -> f64 {
        ratio(
            self.true_positives,
            self.true_positives + self.false_positives,
        )
    }

    #[must_use]
    pub fn recall(self) -> f64 {
        ratio(
            self.true_positives,
            self.true_positives + self.false_negatives,
        )
    }

    /// F-beta. `beta > 1` weights recall above precision; the evaluation uses
    /// 2.0, following presidio-research, because a missed entity leaks while a
    /// false positive merely over-redacts.
    #[must_use]
    pub fn f_beta(self, beta: f64) -> f64 {
        let (p, r) = (self.precision(), self.recall());
        if p == 0.0 && r == 0.0 {
            return 0.0;
        }
        let b2 = beta * beta;
        (1.0 + b2) * p * r / (b2 * p + r)
    }

    fn add(&mut self, other: Self) {
        self.true_positives += other.true_positives;
        self.false_positives += other.false_positives;
        self.false_negatives += other.false_negatives;
    }
}

/// The result of scoring a corpus.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub overall: Counts,
    pub per_category: BTreeMap<String, Counts>,
    pub per_language: BTreeMap<String, Counts>,
    /// Predicted categories with no entry in the [`TaxonomyMap`], and how often
    /// each appeared. **A non-empty map invalidates the score**: those
    /// detections could not be compared, so both precision and recall are
    /// reported against an incomplete picture.
    pub unmapped: BTreeMap<String, usize>,
    /// Predictions whose canonical category is not in the gold scope at all.
    ///
    /// Excluded rather than counted against the model. The gold taxonomy
    /// defines what this comparison measures; a model detecting a `DATE` when
    /// the comparison does not score dates has not made an error, and counting
    /// it as one punishes the model for being more capable than the question.
    pub out_of_scope: BTreeMap<String, usize>,
    /// Gold spans lying beyond the last byte the model's tokenizer covered.
    ///
    /// Unfindable by construction: the text was truncated to the model's
    /// context window before it ever saw them. Counted apart from misses,
    /// because a model with a 512-token window scored on long documents
    /// otherwise looks like a model with poor recall.
    pub unreachable_gold: usize,
    pub documents: usize,
}

impl Report {
    /// Whether the numbers can be trusted, i.e. every predicted category was
    /// mappable.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unmapped.is_empty()
    }

    /// Gold spans the model never had a chance to see, as a share of all gold.
    /// Above a few per cent, the comparison is partly measuring context-window
    /// length rather than detection quality.
    #[must_use]
    pub fn truncation_share(&self) -> f64 {
        let scored = self.overall.true_positives + self.overall.false_negatives;
        ratio(self.unreachable_gold, scored + self.unreachable_gold)
    }
}

/// A prediction mapped into the canonical taxonomy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictedSpan {
    pub category: String,
    pub start: u32,
    pub end: u32,
}

impl PredictedSpan {
    #[must_use]
    pub fn new(category: impl Into<String>, start: u32, end: u32) -> Self {
        Self {
            category: category.into(),
            start,
            end,
        }
    }
}

/// Merges adjacent and overlapping predictions of the same canonical category.
///
/// Necessary, not cosmetic. A model emitting `first_name` and `last_name` as
/// two spans over "Sarah Chen" is correct, but gold annotates one `PERSON`
/// span; under one-to-one matching the second prediction would score as a
/// false positive and the model would be penalised for being more specific
/// than the annotation. A redactor coalesces adjacent spans before applying
/// them anyway, so this measures what would actually happen.
///
/// Only spans separated by at most `gap` bytes are joined, so two genuinely
/// distinct people in a list do not collapse into one.
#[must_use]
pub fn merge_adjacent(mut spans: Vec<PredictedSpan>, gap: u32) -> Vec<PredictedSpan> {
    spans.sort_by(|a, b| (&a.category, a.start).cmp(&(&b.category, b.start)));

    let mut merged: Vec<PredictedSpan> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(prev)
                if prev.category == span.category && span.start <= prev.end.saturating_add(gap) =>
            {
                prev.end = prev.end.max(span.end);
            }
            _ => merged.push(span),
        }
    }
    merged
}

/// The fixed parts of a scoring pass: how categories map, how strictly spans
/// must match, and which canonical categories the corpus scores.
#[derive(Debug, Clone, Copy)]
pub struct Scorer<'a> {
    pub taxonomy: &'a TaxonomyMap,
    pub mode: MatchMode,
    /// The canonical category set of the whole corpus, not of one document.
    /// Per-document scope would excuse a spurious `EMAIL` in any document
    /// that happens to contain no gold email, which is exactly a false
    /// positive — which is why there is no convenience form that derives it
    /// from the document.
    pub in_scope: &'a BTreeSet<String>,
}

impl Scorer<'_> {
    /// Scores one document's predictions against its gold spans.
    ///
    /// Matching is greedy and one-to-one: each gold span consumes at most one
    /// prediction and vice versa. Without that, one sprawling prediction
    /// covering three gold spans would score as three true positives under
    /// [`MatchMode::Overlap`], which flatters a model that fails to separate
    /// adjacent entities.
    ///
    /// `reachable` is the last byte offset the model's tokenizer produced.
    /// Gold beyond it was truncated away before inference and cannot be found
    /// at any quality, so it is counted in [`Report::unreachable_gold`]
    /// instead of as a miss. Pass `None` when the whole document was encoded.
    pub fn score(
        &self,
        doc: &Document,
        predictions: &[Entity],
        reachable: Option<u32>,
        report: &mut Report,
    ) {
        report.documents += 1;

        let mut matched_gold: BTreeSet<usize> = BTreeSet::new();
        let mut per_doc: BTreeMap<String, Counts> = BTreeMap::new();

        // Map into the canonical space first, quarantining anything
        // unmappable, then coalesce: two model categories can map to one
        // canonical one, and it is only after that mapping that they are
        // adjacent spans of one entity.
        let mut canonical_spans: Vec<PredictedSpan> = Vec::with_capacity(predictions.len());
        for prediction in predictions {
            match canonical(self.taxonomy, &prediction.category) {
                Some(category) if self.in_scope.contains(category) => {
                    canonical_spans.push(PredictedSpan::new(
                        category,
                        prediction.start,
                        prediction.end,
                    ));
                }
                Some(category) => {
                    *report.out_of_scope.entry(category.to_owned()).or_default() += 1;
                }
                None => {
                    *report
                        .unmapped
                        .entry(prediction.category.clone())
                        .or_default() += 1;
                }
            }
        }

        for span in merge_adjacent(canonical_spans, ADJACENT_GAP) {
            let hit = doc.spans.iter().enumerate().find(|(i, gold)| {
                !matched_gold.contains(i)
                    && gold.category == span.category
                    && self.mode.accepts(gold, span.start, span.end)
            });

            let entry = per_doc.entry(span.category).or_default();
            match hit {
                Some((i, _)) => {
                    matched_gold.insert(i);
                    entry.true_positives += 1;
                }
                None => entry.false_positives += 1,
            }
        }

        // Every gold span nothing claimed. Counted after the loop rather than
        // during it, so a prediction arriving later can still claim one.
        for (i, gold) in doc.spans.iter().enumerate() {
            if matched_gold.contains(&i) {
                continue;
            }
            if reachable.is_some_and(|limit| gold.start >= limit) {
                report.unreachable_gold += 1;
                continue;
            }
            per_doc
                .entry(gold.category.clone())
                .or_default()
                .false_negatives += 1;
        }

        for (category, counts) in per_doc {
            report.overall.add(counts);
            report.per_category.entry(category).or_default().add(counts);
            if let Some(lang) = &doc.lang {
                report
                    .per_language
                    .entry(lang.clone())
                    .or_default()
                    .add(counts);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Negative controls
// ---------------------------------------------------------------------------

/// What a model flags in text asserted to contain no PII.
///
/// Kept apart from [`Report`] rather than folded into precision, because a
/// large flag rate on clean text can otherwise hide behind good recall on
/// dirty text — and because the two need different interpretation, below.
#[derive(Debug, Clone, Default)]
pub struct NegativeReport {
    pub documents: usize,
    pub chars: usize,
    pub flags: BTreeMap<String, usize>,
}

impl NegativeReport {
    pub fn observe(&mut self, text: &str, predictions: &[Entity]) {
        self.documents += 1;
        self.chars += text.chars().count();
        for prediction in predictions {
            *self.flags.entry(prediction.category.clone()).or_default() += 1;
        }
    }

    #[must_use]
    pub fn total_flags(&self) -> usize {
        self.flags.values().sum()
    }

    /// Flags per ten thousand characters. Characters rather than words or
    /// tokens: word counts are meaningless for unspaced scripts, and token
    /// counts differ per model, which would make the figure incomparable
    /// across exactly the models being compared.
    #[must_use]
    pub fn rate_per_10k(&self, count: usize) -> f64 {
        ratio(count, self.chars) * 10_000.0
    }

    /// Flags in categories that cannot legitimately occur in the corpus.
    ///
    /// The distinction matters and is easy to miss: public-domain literature is
    /// full of *person names and place names*, so a name detector firing on it
    /// is behaving correctly and its flag rate is not an error rate. A detector
    /// reporting an **email address, phone number, IBAN or national identifier**
    /// in a nineteenth-century novel is unambiguously wrong. Only the latter is
    /// a clean false-positive signal, which is why it is counted separately.
    #[must_use]
    pub fn unambiguous_flags(&self, unambiguous: &BTreeSet<String>) -> usize {
        self.flags
            .iter()
            .filter(|(category, _)| unambiguous.contains(*category))
            .map(|(_, n)| *n)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(category: &str, start: u32, end: u32) -> Entity {
        Entity {
            category: category.to_owned(),
            start,
            end,
            confidence: 1.0,
        }
    }

    fn gold(category: &str, start: u32, end: u32) -> GoldSpan {
        GoldSpan {
            category: category.to_owned(),
            start,
            end,
        }
    }

    fn doc(spans: Vec<GoldSpan>) -> Document {
        Document {
            id: "d1".to_owned(),
            text: "0123456789".repeat(10),
            lang: Some("en".to_owned()),
            spans,
        }
    }

    fn taxonomy() -> TaxonomyMap {
        [
            ("email".to_owned(), "EMAIL".to_owned()),
            ("first_name".to_owned(), "PERSON".to_owned()),
            ("last_name".to_owned(), "PERSON".to_owned()),
        ]
        .into_iter()
        .collect()
    }

    /// Corpus scope for tests: every canonical category the map can produce,
    /// which is what a real corpus covering these categories would give.
    fn scope(taxonomy: &TaxonomyMap) -> BTreeSet<String> {
        taxonomy.values().cloned().collect()
    }

    fn score(d: &Document, preds: &[Entity], mode: MatchMode) -> Report {
        let tax = taxonomy();
        let in_scope = scope(&tax);
        let mut report = Report::default();
        Scorer {
            taxonomy: &tax,
            mode,
            in_scope: &in_scope,
        }
        .score(d, preds, None, &mut report);
        report
    }

    #[test]
    fn exact_match_scores_a_true_positive() {
        let r = score(
            &doc(vec![gold("EMAIL", 5, 20)]),
            &[entity("email", 5, 20)],
            MatchMode::Exact,
        );
        assert_eq!(r.overall.true_positives, 1);
        assert_eq!(r.overall.false_positives, 0);
        assert_eq!(r.overall.false_negatives, 0);
        assert!(r.is_complete());
    }

    #[test]
    fn a_boundary_off_by_one_fails_exact_but_passes_overlap() {
        let d = doc(vec![gold("EMAIL", 5, 20)]);
        let preds = [entity("email", 5, 19)];

        let strict = score(&d, &preds, MatchMode::Exact);
        assert_eq!(strict.overall.true_positives, 0);
        assert_eq!(strict.overall.false_positives, 1);
        assert_eq!(strict.overall.false_negatives, 1);

        let loose = score(&d, &preds, MatchMode::Overlap);
        assert_eq!(loose.overall.true_positives, 1);
        assert_eq!(loose.overall.false_negatives, 0);
    }

    #[test]
    fn right_span_wrong_category_is_not_a_match() {
        // Detecting *something* at the right place is not detecting the right
        // thing: a redaction policy keyed on category would treat it wrongly.
        let mut taxonomy = taxonomy();
        taxonomy.insert("phone_number".to_owned(), "PHONE".to_owned());
        let in_scope = scope(&taxonomy);
        let mut r = Report::default();
        Scorer {
            taxonomy: &taxonomy,
            mode: MatchMode::Overlap,
            in_scope: &in_scope,
        }
        .score(
            &doc(vec![gold("EMAIL", 5, 20)]),
            &[entity("phone_number", 5, 20)],
            None,
            &mut r,
        );
        assert_eq!(r.overall.true_positives, 0);
        assert_eq!(r.overall.false_positives, 1);
        assert_eq!(r.overall.false_negatives, 1);
    }

    #[test]
    fn one_sprawling_prediction_cannot_claim_several_gold_spans() {
        // The reason matching is one-to-one. A model that fails to separate
        // adjacent entities must not score as though it found all of them.
        let r = score(
            &doc(vec![gold("PERSON", 0, 5), gold("PERSON", 6, 11)]),
            &[entity("first_name", 0, 11)],
            MatchMode::Overlap,
        );
        assert_eq!(r.overall.true_positives, 1);
        assert_eq!(
            r.overall.false_negatives, 1,
            "the second gold span is still missed"
        );
    }

    #[test]
    fn adjacent_predictions_of_one_entity_are_coalesced() {
        // "Sarah Chen" as first_name + last_name is one PERSON, not one hit
        // and one false positive.
        let r = score(
            &doc(vec![gold("PERSON", 0, 11)]),
            &[entity("first_name", 0, 5), entity("last_name", 6, 11)],
            MatchMode::Exact,
        );
        assert_eq!(r.overall.true_positives, 1);
        assert_eq!(
            r.overall.false_positives, 0,
            "the model was not more wrong for being more specific"
        );
    }

    #[test]
    fn a_wide_gap_keeps_distinct_entities_apart() {
        // Two people in a list must not collapse into one span.
        let merged = merge_adjacent(
            vec![
                PredictedSpan::new("PERSON", 0, 5),
                PredictedSpan::new("PERSON", 20, 25),
            ],
            ADJACENT_GAP,
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn different_categories_never_merge() {
        let merged = merge_adjacent(
            vec![
                PredictedSpan::new("PERSON", 0, 5),
                PredictedSpan::new("EMAIL", 6, 11),
            ],
            ADJACENT_GAP,
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn two_predictions_cannot_both_claim_one_gold_span() {
        // Far enough apart not to be coalesced, so the second really is spare.
        let r = score(
            &doc(vec![gold("PERSON", 0, 5)]),
            &[entity("first_name", 0, 5), entity("last_name", 40, 45)],
            MatchMode::Overlap,
        );
        assert_eq!(r.overall.true_positives, 1);
        assert_eq!(r.overall.false_positives, 1);
    }

    #[test]
    fn a_missed_entity_is_a_false_negative() {
        let r = score(&doc(vec![gold("EMAIL", 5, 20)]), &[], MatchMode::Exact);
        assert_eq!(r.overall.false_negatives, 1);
        assert!((r.overall.recall() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn unmapped_categories_are_quarantined_not_scored() {
        // An incomplete taxonomy map is a defect in the measurement. Scoring
        // such a prediction as a false positive would blame the model for it.
        let r = score(
            &doc(vec![gold("EMAIL", 5, 20)]),
            &[entity("SOMETHING_UNMAPPED", 5, 20)],
            MatchMode::Exact,
        );
        assert_eq!(r.overall.false_positives, 0, "not blamed on the model");
        assert_eq!(r.unmapped.get("SOMETHING_UNMAPPED"), Some(&1));
        assert!(!r.is_complete(), "the report must declare itself unusable");
    }

    #[test]
    fn f_beta_weights_recall_above_precision_at_beta_2() {
        // Same counts, mirrored: the case that misses less scores higher.
        let misses_more = Counts {
            true_positives: 5,
            false_positives: 1,
            false_negatives: 5,
        };
        let misses_less = Counts {
            true_positives: 5,
            false_positives: 5,
            false_negatives: 1,
        };
        assert!(misses_less.f_beta(2.0) > misses_more.f_beta(2.0));
    }

    #[test]
    fn empty_counts_do_not_divide_by_zero() {
        let empty = Counts::default();
        assert!((empty.precision() - 0.0).abs() < f64::EPSILON);
        assert!((empty.recall() - 0.0).abs() < f64::EPSILON);
        assert!((empty.f_beta(2.0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn negative_controls_separate_ambiguous_from_unambiguous_flags() {
        // A name in a novel is the detector working. An email address in one
        // is not.
        let mut neg = NegativeReport::default();
        neg.observe(
            &"x".repeat(10_000),
            &[
                entity("first_name", 0, 5),
                entity("first_name", 6, 11),
                entity("email", 20, 30),
            ],
        );

        let unambiguous: BTreeSet<String> = ["email".to_owned()].into_iter().collect();
        assert_eq!(neg.total_flags(), 3);
        assert_eq!(neg.unambiguous_flags(&unambiguous), 1);
        assert!((neg.rate_per_10k(neg.total_flags()) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn overlapping_and_touching_spans_merge_into_their_union() {
        let merged = merge_adjacent(
            vec![
                PredictedSpan::new("PERSON", 6, 11),
                PredictedSpan::new("PERSON", 0, 8),
                PredictedSpan::new("PERSON", 12, 15),
            ],
            ADJACENT_GAP,
        );
        assert_eq!(merged, vec![PredictedSpan::new("PERSON", 0, 15)]);
    }

    #[test]
    fn gold_beyond_the_reachable_offset_is_unreachable_not_missed() {
        let tax = taxonomy();
        let in_scope = scope(&tax);
        let mut r = Report::default();
        Scorer {
            taxonomy: &tax,
            mode: MatchMode::Exact,
            in_scope: &in_scope,
        }
        .score(
            &doc(vec![gold("EMAIL", 5, 20), gold("EMAIL", 60, 70)]),
            &[],
            Some(50),
            &mut r,
        );
        assert_eq!(
            r.overall.false_negatives, 1,
            "only the reachable one is a miss"
        );
        assert_eq!(r.unreachable_gold, 1);
        assert!((r.truncation_share() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn out_of_scope_predictions_are_excluded_not_false_positives() {
        let mut tax = taxonomy();
        tax.insert("date".to_owned(), "DATE".to_owned());
        // DATE maps, but the corpus does not score it.
        let in_scope = scope(&taxonomy());
        let mut r = Report::default();
        Scorer {
            taxonomy: &tax,
            mode: MatchMode::Overlap,
            in_scope: &in_scope,
        }
        .score(&doc(vec![]), &[entity("date", 0, 4)], None, &mut r);
        assert_eq!(r.overall.false_positives, 0);
        assert_eq!(r.out_of_scope.get("DATE"), Some(&1));
        assert!(r.is_complete(), "out of scope is not unmapped");
    }

    #[test]
    fn an_empty_taxonomy_is_identity() {
        let empty = TaxonomyMap::new();
        let in_scope: BTreeSet<String> = ["email".to_owned()].into_iter().collect();
        let mut r = Report::default();
        Scorer {
            taxonomy: &empty,
            mode: MatchMode::Exact,
            in_scope: &in_scope,
        }
        .score(
            &doc(vec![gold("email", 5, 20)]),
            &[entity("email", 5, 20)],
            None,
            &mut r,
        );
        assert_eq!(r.overall.true_positives, 1);
        assert!(r.is_complete());
    }

    #[test]
    fn per_language_counts_follow_the_document() {
        let r = score(
            &doc(vec![gold("EMAIL", 5, 20)]),
            &[entity("email", 5, 20)],
            MatchMode::Exact,
        );
        assert_eq!(r.per_language.get("en").map(|c| c.true_positives), Some(1));
        assert_eq!(
            r.per_category.get("EMAIL").map(|c| c.true_positives),
            Some(1)
        );
        assert_eq!(r.documents, 1);
    }
}
