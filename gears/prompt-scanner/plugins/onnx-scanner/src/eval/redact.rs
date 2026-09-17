//! Loads the REDACT benchmark into the evaluation's own corpus shape.
//!
//! REDACT is a systematically controlled multilingual benchmark for personal
//! information detection — 25 languages, 51 canonical entity types, generated
//! with controlled variation across nine axes. See
//! <https://github.com/guneeshvats/REDACT-PII-Benchmark> and
//! <https://arxiv.org/abs/2606.19881>. Its dataset terms permit benchmarking
//! and defensive PII-detection work; the corpus is synthetic.
//!
//! Two conversions happen here, and both are the kind that produce plausible
//! wrong numbers if skipped:
//!
//! 1. **Character offsets become byte offsets.** REDACT indexes by character;
//!    [`Entity`](crate::domain::entity::Entity) and therefore every backend
//!    indexes by byte. On a Hebrew or Chinese document the two diverge
//!    immediately, and comparing them would score almost every span as a miss
//!    while looking like a model failure.
//! 2. **51 gold types collapse into the comparison's taxonomy.** No model here
//!    distinguishes a work email from a personal one, so scoring against types
//!    at that granularity measures the annotation scheme rather than the model.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use super::scoring::{Document, GoldSpan, TaxonomyMap};

#[derive(Debug, Deserialize)]
struct RedactRecord {
    record_id: i64,
    text: String,
    axes: Axes,
    entities: Vec<RedactEntity>,
}

#[derive(Debug, Deserialize)]
struct Axes {
    language: String,
}

#[derive(Debug, Deserialize)]
struct RedactEntity {
    entity_type: String,
    entity_string: String,
    /// Character offsets into `text`, not bytes. Null for a small number of
    /// records the generator could not place; treated as misannotated.
    start: Option<usize>,
    end: Option<usize>,
    #[serde(default)]
    repair_status: String,
}

/// What the conversion dropped, and why. Reported rather than returned
/// silently: each line of it is a reason the corpus scored against is smaller
/// than the corpus that was published.
#[derive(Debug, Default)]
pub struct LoadStats {
    pub records: usize,
    pub entities_kept: usize,
    /// Annotations REDACT itself marks `string_not_in_text`, plus any whose
    /// offsets do not actually contain the annotated string. Upstream defects,
    /// not ours, but they must not be scored as misses.
    pub entities_misannotated: usize,
    /// Gold types with no entry in the gold taxonomy, and how many spans each
    /// lost. **These are out of scope for the comparison, not misses** — no
    /// model is asked about them, so counting them would depress every score
    /// equally and mean nothing.
    pub excluded_types: BTreeMap<String, usize>,
}

/// Reads REDACT JSON, coarsening gold types through `gold_taxonomy`.
///
/// An empty `gold_taxonomy` keeps every type as-is, which is only useful for
/// inspecting the corpus: no model in this repo speaks all 51.
///
/// # Errors
///
/// If the file cannot be read or is not the expected JSON array.
pub fn load(
    path: &Path,
    gold_taxonomy: &TaxonomyMap,
) -> Result<(Vec<Document>, LoadStats), String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let records: Vec<RedactRecord> =
        serde_json::from_str(&raw).map_err(|e| format!("parsing {}: {e}", path.display()))?;

    let mut stats = LoadStats::default();
    let mut documents = Vec::with_capacity(records.len());

    for record in records {
        stats.records += 1;
        let offsets = ByteOffsets::of(&record.text);
        let mut spans = Vec::with_capacity(record.entities.len());

        for entity in record.entities {
            // REDACT flags its own bad annotations; the slice check catches any
            // it missed. Either way the span cannot be compared against.
            let placed = entity
                .start
                .zip(entity.end)
                .and_then(|(s, e)| offsets.byte_range(s, e));
            let Some((start, end)) = placed else {
                stats.entities_misannotated += 1;
                continue;
            };
            if entity.repair_status == "string_not_in_text"
                || record.text.get(start..end) != Some(entity.entity_string.as_str())
            {
                stats.entities_misannotated += 1;
                continue;
            }

            let category = if gold_taxonomy.is_empty() {
                entity.entity_type.clone()
            } else if let Some(canonical) = gold_taxonomy.get(&entity.entity_type) {
                canonical.clone()
            } else {
                *stats.excluded_types.entry(entity.entity_type).or_default() += 1;
                continue;
            };

            stats.entities_kept += 1;
            spans.push(GoldSpan {
                category,
                start: u32::try_from(start).unwrap_or(u32::MAX),
                end: u32::try_from(end).unwrap_or(u32::MAX),
            });
        }

        // A record whose every span was excluded is dropped rather than kept as
        // a negative control: nothing asserts its text is PII-free — it is full
        // of PII the comparison simply does not cover.
        if spans.is_empty() {
            continue;
        }

        documents.push(Document {
            id: record.record_id.to_string(),
            text: record.text,
            lang: Some(record.axes.language),
            spans,
        });
    }

    Ok((documents, stats))
}

/// Character index to byte offset, computed once per document.
///
/// Built rather than repeatedly calling `char_indices`, because a REDACT
/// document runs to thousands of characters with dozens of entities and the
/// naive form is quadratic.
struct ByteOffsets(Vec<usize>);

impl ByteOffsets {
    fn of(text: &str) -> Self {
        let mut offsets: Vec<usize> = text.char_indices().map(|(byte, _)| byte).collect();
        // One past the end, so a span ending at the last character resolves.
        offsets.push(text.len());
        Self(offsets)
    }

    fn byte_range(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        if start > end {
            return None;
        }
        Some((*self.0.get(start)?, *self.0.get(end)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_offsets_are_unchanged() {
        let offsets = ByteOffsets::of("Ticket: REG-2024");
        assert_eq!(offsets.byte_range(8, 16), Some((8, 16)));
    }

    #[test]
    fn multibyte_text_shifts_offsets() {
        // Cyrillic: every character is two bytes, so character 3 is byte 6.
        // Treating REDACT's character offsets as bytes would slice mid-character
        // here and mismatch every span in the document.
        let text = "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} x";
        let offsets = ByteOffsets::of(text);
        assert_eq!(offsets.byte_range(0, 6), Some((0, 12)));
        assert_eq!(&text[0..12], "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}");
    }

    #[test]
    fn a_span_ending_at_the_last_character_resolves() {
        let text = "abc";
        assert_eq!(ByteOffsets::of(text).byte_range(0, 3), Some((0, 3)));
    }

    #[test]
    fn an_out_of_range_span_is_rejected_rather_than_clamped() {
        assert_eq!(ByteOffsets::of("abc").byte_range(0, 99), None);
        assert_eq!(ByteOffsets::of("abc").byte_range(3, 1), None);
    }

    fn load_str(json: &str, gold: &TaxonomyMap) -> (Vec<Document>, LoadStats) {
        use std::io::Write as _;
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        file.write_all(json.as_bytes()).expect("write");
        load(file.path(), gold).expect("loads")
    }

    #[test]
    fn load_converts_keeps_and_counts_what_it_drops() {
        // Record 1: one good EMAIL, one whose offsets do not hold its string,
        // one flagged upstream, one unplaced, one out-of-scope type.
        // Record 2: only an out-of-scope type, so the record is dropped.
        let json = r#"[
          {"record_id": 1, "text": "\u043f\u0440 a@b.co x", "axes": {"language": "ru"},
           "entities": [
             {"entity_type": "EMAIL_WORK", "entity_string": "a@b.co", "start": 3, "end": 9},
             {"entity_type": "EMAIL_WORK", "entity_string": "zzz", "start": 3, "end": 6},
             {"entity_type": "EMAIL_WORK", "entity_string": "a@b.co", "start": 3, "end": 9,
              "repair_status": "string_not_in_text"},
             {"entity_type": "EMAIL_WORK", "entity_string": "a", "start": null, "end": null},
             {"entity_type": "DATE", "entity_string": "x", "start": 10, "end": 11}
           ]},
          {"record_id": 2, "text": "x", "axes": {"language": "en"},
           "entities": [{"entity_type": "DATE", "entity_string": "x", "start": 0, "end": 1}]}
        ]"#;
        let gold: TaxonomyMap = [("EMAIL_WORK".to_owned(), "EMAIL".to_owned())]
            .into_iter()
            .collect();
        let (docs, stats) = load_str(json, &gold);

        assert_eq!(stats.records, 2);
        assert_eq!(stats.entities_kept, 1);
        assert_eq!(stats.entities_misannotated, 3);
        assert_eq!(stats.excluded_types.get("DATE"), Some(&2));

        assert_eq!(
            docs.len(),
            1,
            "the all-excluded record is dropped, not a negative control"
        );
        let doc = &docs[0];
        assert_eq!(doc.id, "1");
        assert_eq!(doc.lang.as_deref(), Some("ru"));
        // Character 3 is byte 5: two 2-byte Cyrillic characters and a space.
        assert_eq!(doc.spans[0].category, "EMAIL");
        assert_eq!((doc.spans[0].start, doc.spans[0].end), (5, 11));
        assert_eq!(&doc.text[5..11], "a@b.co");
    }

    #[test]
    fn an_empty_gold_taxonomy_keeps_every_type() {
        let json = r#"[{"record_id": 7, "text": "x", "axes": {"language": "en"},
           "entities": [{"entity_type": "DATE", "entity_string": "x", "start": 0, "end": 1}]}]"#;
        let (docs, stats) = load_str(json, &TaxonomyMap::new());
        assert_eq!(stats.entities_kept, 1);
        assert_eq!(docs[0].spans[0].category, "DATE");
    }

    #[test]
    fn malformed_json_is_an_error() {
        use std::io::Write as _;
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        file.write_all(b"{not json").expect("write");
        assert!(load(file.path(), &TaxonomyMap::new()).is_err());
    }
}
