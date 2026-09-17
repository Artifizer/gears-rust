//! What a flagged scan stores: one [`EvidenceRow`] per finding.
//!
//! Pure functions, no I/O: `build_rows` turns a scan's findings into rows
//! (ordinal, expiry, optional excerpt), skipping findings the schema cannot
//! hold rather than failing the scan. See `docs/DESIGN.md`, "Storage".

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use prompt_scanner_sdk::{ContentKind, EvidencePolicy, Finding, Span, VerdictAction};

use crate::config::EvidenceConfig;
use crate::domain::repos::{EvidenceRow, Excerpt};
use uuid::Uuid;

/// Longest excerpt, in characters (Unicode code points, as PostgreSQL counts).
pub(crate) const EXCERPT_MAX_CHARS: usize = 2048;
/// Longest excerpt, in UTF-8 bytes.
pub(crate) const EXCERPT_MAX_BYTES: usize = 8192;

const DETECTOR_MAX_CHARS: usize = 1024;
const DETECTOR_VERSION_MAX_CHARS: usize = 64;
const CATEGORY_MAX_CHARS: usize = 128;
const SETTINGS_SCHEMA_MAX_CHARS: usize = 16;

/// The scan-level facts every finding row of a scan repeats.
#[derive(Debug, Clone)]
pub(crate) struct ScanFacts {
    pub trace_id: Uuid,
    pub tenant_id: Uuid,
    pub correlation_id: Option<String>,
    pub connector_id: String,
    pub content_kind: ContentKind,
    pub created_at: DateTime<Utc>,
    pub settings_schema: Option<String>,
    /// The policy's retention, `None` when it sets none.
    pub retention: Option<Duration>,
    pub evidence: EvidencePolicy,
    /// The scanned text was extracted from a file: excerpts are always
    /// windows, never the whole text.
    pub from_file: bool,
}

/// Why a finding was not stored. The metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotStored {
    /// Past `max_findings_stored` for the scan.
    OverCap,
    /// A scanner value longer than its column, or out of its `CHECK` range:
    /// a plugin defect.
    InvalidValue,
    /// The write queue was full; the scan's rows were dropped.
    QueueFull,
}

impl NotStored {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::OverCap => "over_cap",
            Self::InvalidValue => "invalid_value",
            Self::QueueFull => "queue_full",
        }
    }
}

/// The rows to insert, and how many findings were left out and why.
#[derive(Debug, Default)]
pub(crate) struct BuiltRows {
    pub rows: Vec<EvidenceRow>,
    pub over_cap: u64,
    pub invalid: u64,
}

/// When a row expires: the policy's retention clamped to `max_retention`, and
/// to `max_evidence_retention` when the row holds an excerpt.
pub(crate) fn expires_at(
    created_at: DateTime<Utc>,
    retention: Option<Duration>,
    has_excerpt: bool,
    cfg: &EvidenceConfig,
) -> DateTime<Utc> {
    let mut keep = retention
        .filter(|r| !r.is_zero())
        .unwrap_or(cfg.max_retention)
        .min(cfg.max_retention);
    if has_excerpt {
        keep = keep.min(cfg.max_evidence_retention);
    }
    TimeDelta::from_std(keep)
        .ok()
        .and_then(|d| created_at.checked_add_signed(d))
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// Build the rows for `findings`, numbering them from `first_ordinal`.
///
/// `action` is the scan's verdict when the rows are written. `text` is the
/// scanned text, used for excerpts when the policy enables evidence for
/// `action`.
pub(crate) fn build_rows(
    facts: &ScanFacts,
    action: VerdictAction,
    findings: &[Finding],
    text: &str,
    first_ordinal: u16,
    cfg: &EvidenceConfig,
) -> BuiltRows {
    let with_excerpt = facts.evidence.applies_to(action);
    // `Service::validate_policy` refuses a longer one, for written and
    // provider-served policies alike; this only keeps the insert valid.
    let settings_schema = facts
        .settings_schema
        .clone()
        .filter(|s| s.chars().count() <= SETTINGS_SCHEMA_MAX_CHARS);
    let cap = cfg
        .max_findings_stored
        .min(crate::config::MAX_FINDINGS_STORED_LIMIT);

    let mut out = BuiltRows::default();
    let mut next = first_ordinal;
    for finding in findings {
        if !fits_schema(finding) {
            out.invalid += 1;
            continue;
        }
        let Some(ordinal) = (next < cap).then(|| u8::try_from(next).ok()).flatten() else {
            out.over_cap += 1;
            continue;
        };
        next += 1;

        let excerpt = with_excerpt
            .then(|| {
                excerpt(
                    text,
                    finding.span,
                    facts.from_file,
                    cfg.evidence_context_chars,
                )
            })
            .flatten();
        out.rows.push(EvidenceRow {
            trace_id: facts.trace_id,
            ordinal,
            tenant_id: facts.tenant_id,
            correlation_id: facts.correlation_id.clone(),
            connector_id: facts.connector_id.clone(),
            content_kind: facts.content_kind,
            created_at: facts.created_at,
            expires_at: expires_at(facts.created_at, facts.retention, excerpt.is_some(), cfg),
            action,
            settings_schema: settings_schema.clone(),
            finding: finding.clone(),
            excerpt,
        });
    }
    out
}

/// Whether the schema can hold this finding: column lengths and `CHECK`s.
fn fits_schema(f: &Finding) -> bool {
    let span_ok = f.span.is_none_or(|s| {
        s.end >= s.start && i32::try_from(s.start).is_ok() && i32::try_from(s.end).is_ok()
    });
    !f.detector.is_empty()
        && f.detector.chars().count() <= DETECTOR_MAX_CHARS
        && f.category.chars().count() <= CATEGORY_MAX_CHARS
        && f.detector_version
            .as_ref()
            .is_none_or(|v| v.chars().count() <= DETECTOR_VERSION_MAX_CHARS)
        && (0.0..=1.0).contains(&f.confidence)
        && span_ok
}

/// The text stored around a finding.
///
/// - Not from a file, and at most [`EXCERPT_MAX_CHARS`] characters and
///   [`EXCERPT_MAX_BYTES`] bytes: the whole text.
/// - No span: the beginning of the text, up to the limits.
/// - Otherwise: `context_chars` characters on each side of the span, cut to
///   the limits (a long span keeps only its beginning).
///
/// Cuts fall on UTF-8 character boundaries. Each NUL becomes a space, which
/// keeps every byte offset valid. `None` only for an offset that does not fit
/// the `INTEGER` column.
pub(crate) fn excerpt(
    text: &str,
    span: Option<Span>,
    from_file: bool,
    context_chars: usize,
) -> Option<Excerpt> {
    let (begin, end) = match span {
        _ if !from_file && fits_whole(text) => (0, text.len()),
        None => (0, text.len()),
        Some(span) => {
            let len = text.len();
            let start = floor_boundary(text, (span.start as usize).min(len));
            let stop = ceil_boundary(text, (span.end as usize).clamp(start, len));
            (
                chars_back(text, start, context_chars),
                chars_forward(text, stop, context_chars),
            )
        }
    };
    let end = capped_end(text, begin, end);
    let start = u32::try_from(begin)
        .ok()
        .filter(|s| i32::try_from(*s).is_ok())?;
    Some(Excerpt {
        text: text[begin..end].replace('\0', " "),
        start,
    })
}

fn fits_whole(text: &str) -> bool {
    text.len() <= EXCERPT_MAX_BYTES && text.chars().count() <= EXCERPT_MAX_CHARS
}

fn floor_boundary(text: &str, mut i: usize) -> usize {
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(text: &str, mut i: usize) -> usize {
    while !text.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// The byte offset `n` characters before `pos` (or 0).
fn chars_back(text: &str, pos: usize, n: usize) -> usize {
    if n == 0 {
        return pos;
    }
    text[..pos]
        .char_indices()
        .rev()
        .take(n)
        .last()
        .map_or(pos, |(i, _)| i)
}

/// The byte offset `n` characters after `pos` (or the end).
fn chars_forward(text: &str, pos: usize, n: usize) -> usize {
    text[pos..]
        .char_indices()
        .nth(n)
        .map_or(text.len(), |(i, _)| pos + i)
}

/// The end of the longest prefix of `text[begin..end]` within the excerpt
/// limits.
fn capped_end(text: &str, begin: usize, end: usize) -> usize {
    let mut cut = begin;
    for (count, (i, c)) in text[begin..end].char_indices().enumerate() {
        let next = begin + i + c.len_utf8();
        if count >= EXCERPT_MAX_CHARS || next - begin > EXCERPT_MAX_BYTES {
            break;
        }
        cut = next;
    }
    cut
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use prompt_scanner_sdk::Severity;

    #[expect(clippy::unnecessary_wraps, reason = "call sites pass an Option<Span>")]
    fn span(start: u32, end: u32) -> Option<Span> {
        Some(Span { start, end })
    }

    #[test]
    fn short_text_is_stored_whole() {
        let e = excerpt("call me at 555-1234", span(11, 19), false, 4).unwrap();
        assert_eq!(e.text, "call me at 555-1234");
        assert_eq!(e.start, 0);
    }

    #[test]
    fn file_text_is_always_a_window() {
        let e = excerpt("call me at 555-1234 today", span(11, 19), true, 3).unwrap();
        assert_eq!(e.text, "at 555-1234 to");
        assert_eq!(e.start, 8);
    }

    #[test]
    fn long_text_is_a_window_around_the_span() {
        let text = format!("{}SECRET{}", "a".repeat(3000), "b".repeat(3000));
        let e = excerpt(&text, span(3000, 3006), false, 256).unwrap();
        assert_eq!(e.start, 3000 - 256);
        assert_eq!(e.text.len(), 256 + 6 + 256);
        assert!(e.text.contains("SECRET"));
    }

    #[test]
    fn no_span_stores_the_beginning() {
        let text = "x".repeat(5000);
        let e = excerpt(&text, None, false, 256).unwrap();
        assert_eq!(e.start, 0);
        assert_eq!(e.text.chars().count(), EXCERPT_MAX_CHARS);
    }

    #[test]
    fn a_long_span_keeps_only_its_beginning() {
        let text = "y".repeat(10_000);
        let e = excerpt(&text, span(1000, 9000), false, 256).unwrap();
        assert_eq!(e.start, 1000 - 256);
        assert_eq!(e.text.chars().count(), EXCERPT_MAX_CHARS);
    }

    #[test]
    fn cuts_fall_on_char_boundaries_and_respect_the_byte_limit() {
        // 4-byte characters: 2,048 of them would be 8,192 bytes exactly.
        let text = "\u{1F600}".repeat(3000);
        // A span starting inside a character.
        let e = excerpt(&text, span(4001, 4002), false, 10).unwrap();
        assert!(text.is_char_boundary(e.start as usize));
        assert!(e.text.len() <= EXCERPT_MAX_BYTES);
        assert!(e.text.chars().count() <= EXCERPT_MAX_CHARS);

        let e = excerpt(&text, None, false, 10).unwrap();
        assert_eq!(e.text.len(), EXCERPT_MAX_BYTES);
    }

    #[test]
    fn nul_bytes_become_spaces_without_moving_offsets() {
        let e = excerpt("a\0b", span(0, 1), false, 1).unwrap();
        assert_eq!(e.text, "a b");
    }

    #[test]
    fn a_span_past_the_end_is_clamped() {
        let text = "z".repeat(3000);
        let e = excerpt(&text, span(9000, 9999), false, 5).unwrap();
        assert_eq!(e.start, 2995);
        assert_eq!(e.text, "zzzzz");
    }

    fn cfg() -> EvidenceConfig {
        EvidenceConfig::default()
    }

    const DAY: Duration = Duration::from_hours(24);

    #[test]
    fn retention_is_clamped_and_evidence_lives_shorter() {
        let now = Utc::now();
        let days = |d: i64| now + TimeDelta::days(d);
        assert_eq!(expires_at(now, None, false, &cfg()), days(90));
        assert_eq!(expires_at(now, Some(DAY * 7), false, &cfg()), days(7));
        assert_eq!(expires_at(now, Some(DAY * 400), false, &cfg()), days(90));
        assert_eq!(expires_at(now, Some(DAY * 60), true, &cfg()), days(30));
        assert_eq!(
            expires_at(now, Some(Duration::ZERO), false, &cfg()),
            days(90)
        );
    }

    fn finding(detector: &str, category: &str) -> Finding {
        Finding {
            detector: detector.to_owned(),
            category: category.to_owned(),
            severity: Severity::High,
            confidence: 0.9,
            span: span(0, 4),
            suggested_action: VerdictAction::Block,
            detector_version: Some("1.0.0".to_owned()),
        }
    }

    fn facts(evidence: EvidencePolicy) -> ScanFacts {
        ScanFacts {
            trace_id: Uuid::now_v7(),
            tenant_id: Uuid::from_u128(7),
            correlation_id: Some("req-1".to_owned()),
            connector_id: "test".to_owned(),
            content_kind: ContentKind::Prompt,
            created_at: Utc::now(),
            settings_schema: Some("1.0".to_owned()),
            retention: None,
            evidence,
            from_file: false,
        }
    }

    #[test]
    fn rows_are_numbered_capped_and_invalid_ones_skipped() {
        let mut bad_confidence = finding("gts.a", "pii.email");
        bad_confidence.confidence = 1.5;
        let findings = vec![
            finding("gts.a", "pii.email"),
            finding(&"d".repeat(1025), "pii.email"),
            bad_confidence,
            finding("gts.a", "pii.phone"),
            finding("gts.a", "pii.name"),
        ];
        let cfg = EvidenceConfig {
            max_findings_stored: 3,
            ..EvidenceConfig::default()
        };
        let built = build_rows(
            &facts(EvidencePolicy::default()),
            VerdictAction::Block,
            &findings,
            "text",
            1,
            &cfg,
        );
        let ordinals: Vec<u8> = built.rows.iter().map(|r| r.ordinal).collect();
        assert_eq!(ordinals, [1, 2]);
        assert_eq!(built.invalid, 2);
        assert_eq!(built.over_cap, 1);
        assert!(built.rows.iter().all(|r| r.excerpt.is_none()));
    }

    #[test]
    fn excerpts_follow_the_evidence_policy() {
        let only_block = EvidencePolicy {
            enabled: true,
            actions: Some(vec![VerdictAction::Block]),
        };
        let findings = [finding("gts.a", "secret")];
        let built = build_rows(
            &facts(only_block.clone()),
            VerdictAction::Block,
            &findings,
            "abcd efgh",
            0,
            &cfg(),
        );
        let row = &built.rows[0];
        assert_eq!(row.excerpt.as_ref().unwrap().text, "abcd efgh");
        assert_eq!(row.expires_at, row.created_at + TimeDelta::days(30));

        let built = build_rows(
            &facts(only_block),
            VerdictAction::Warn,
            &findings,
            "abcd efgh",
            0,
            &cfg(),
        );
        assert!(built.rows[0].excerpt.is_none());
        assert_eq!(
            built.rows[0].expires_at,
            built.rows[0].created_at + TimeDelta::days(90)
        );
    }

    /// Each column limit and `CHECK` the schema enforces, one finding at a
    /// time: a finding that breaks one is skipped and counted, never sent to
    /// the database.
    #[test]
    fn findings_the_schema_cannot_hold_are_skipped() {
        type Breaker = Box<dyn Fn(&mut Finding)>;
        let breakers: Vec<(&str, Breaker)> = vec![
            ("empty detector", Box::new(|f| f.detector.clear())),
            (
                "long detector",
                Box::new(|f| f.detector = "d".repeat(DETECTOR_MAX_CHARS + 1)),
            ),
            (
                "long category",
                Box::new(|f| f.category = "c".repeat(CATEGORY_MAX_CHARS + 1)),
            ),
            (
                "long version",
                Box::new(|f| {
                    f.detector_version = Some("v".repeat(DETECTOR_VERSION_MAX_CHARS + 1));
                }),
            ),
            ("NaN confidence", Box::new(|f| f.confidence = f32::NAN)),
            ("negative confidence", Box::new(|f| f.confidence = -0.1)),
            ("span end before start", Box::new(|f| f.span = span(5, 4))),
            (
                "span past INTEGER",
                Box::new(|f| f.span = span(u32::MAX - 1, u32::MAX)),
            ),
        ];
        for (what, breaker) in breakers {
            let mut f = finding("gts.a", "pii.email");
            breaker(&mut f);
            let built = build_rows(
                &facts(EvidencePolicy::default()),
                VerdictAction::Block,
                &[f],
                "text",
                0,
                &cfg(),
            );
            assert!(built.rows.is_empty(), "{what}");
            assert_eq!(built.invalid, 1, "{what}");
        }

        // At the limits, it is stored.
        let mut f = finding(
            &"d".repeat(DETECTOR_MAX_CHARS),
            &"c".repeat(CATEGORY_MAX_CHARS),
        );
        f.detector_version = Some("v".repeat(DETECTOR_VERSION_MAX_CHARS));
        f.confidence = 1.0;
        let built = build_rows(
            &facts(EvidencePolicy::default()),
            VerdictAction::Block,
            &[f],
            "text",
            0,
            &cfg(),
        );
        assert_eq!(built.rows.len(), 1);
    }

    #[test]
    fn a_settings_schema_too_long_for_its_column_is_left_out() {
        let mut facts = facts(EvidencePolicy::default());
        facts.settings_schema = Some("x".repeat(SETTINGS_SCHEMA_MAX_CHARS + 1));
        let built = build_rows(
            &facts,
            VerdictAction::Block,
            &[finding("gts.a", "pii")],
            "text",
            0,
            &cfg(),
        );
        assert_eq!(built.rows[0].settings_schema, None);
    }

    proptest::proptest! {
        /// Any text and any span: no panic, the excerpt is a slice of the text
        /// starting on a character boundary (NULs as spaces) and within both
        /// limits.
        #[test]
        fn an_excerpt_is_always_a_bounded_slice_of_the_text(
            text in "(?s).{0,3000}",
            start in 0_u32..12_000,
            len in 0_u32..12_000,
            with_span in proptest::bool::ANY,
            from_file in proptest::bool::ANY,
            context in 0_usize..300,
        ) {
            let span = with_span.then(|| Span { start, end: start.saturating_add(len) });
            let e = excerpt(&text, span, from_file, context).expect("offsets fit");
            let at = e.start as usize;
            proptest::prop_assert!(text.is_char_boundary(at));
            proptest::prop_assert!(e.text.len() <= EXCERPT_MAX_BYTES);
            proptest::prop_assert!(e.text.chars().count() <= EXCERPT_MAX_CHARS);
            let source = text[at..].replace('\0', " ");
            proptest::prop_assert!(source.starts_with(&e.text));
        }
    }
}
