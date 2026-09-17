//! Span-based redaction: replaces only the detected `Redact`-suggesting
//! spans within a payload, instead of blanking the whole thing. Shared
//! between `prompt-scanner`'s own audit-trail redaction and connectors that
//! need to modify a request body in place, so both apply the exact same
//! span-merging/fallback rules.

use crate::models::{Finding, VerdictAction};

/// Placeholder substituted for each redacted span (or for the whole text,
/// in the fallback case below).
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// Replaces every `Redact`-suggesting finding's span in `text` with
/// [`REDACTED_PLACEHOLDER`], leaving the rest of the text untouched.
/// Overlapping/adjacent spans are merged into one placeholder rather than
/// producing back-to-back duplicates.
///
/// Falls back to redacting the *entire* text (never leaves anything
/// unredacted) when:
/// - any `Redact`-suggesting finding has no span (can't be surgical about
///   something with no location), or
/// - any span is malformed (out of bounds, inverted, or not on a UTF-8 char
///   boundary — byte offsets should always land cleanly here since they
///   come from regex/tokenizer matches against this exact text, but this
///   is the failure-safe path if that ever isn't true).
///
/// Returns `text` unchanged if there are no `Redact`-suggesting findings at
/// all (callers are expected to only reach for this when the aggregated
/// verdict action is `Redact`, but this makes the function correct
/// regardless).
#[must_use]
pub fn redact_spans(text: &str, findings: &[Finding]) -> String {
    let redact_findings: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.suggested_action == VerdictAction::Redact)
        .collect();

    if redact_findings.is_empty() {
        return text.to_owned();
    }
    if redact_findings.iter().any(|f| f.span.is_none()) {
        return REDACTED_PLACEHOLDER.to_owned();
    }

    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(redact_findings.len());
    for span in redact_findings.iter().filter_map(|f| f.span) {
        let (start, end) = (span.start as usize, span.end as usize);
        let well_formed = start <= end
            && end <= text.len()
            && text.is_char_boundary(start)
            && text.is_char_boundary(end);
        if !well_formed {
            // One bad span means we cannot know what it was meant to cover:
            // fail safe rather than leak it.
            return REDACTED_PLACEHOLDER.to_owned();
        }
        spans.push((start, end));
    }

    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }

    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, end) in merged {
        result.push_str(&text[cursor..start]);
        result.push_str(REDACTED_PLACEHOLDER);
        cursor = end;
    }
    result.push_str(&text[cursor..]);
    result
}

#[cfg(test)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "test fixtures are short strings"
)]
mod tests {
    use super::*;
    use crate::models::{Severity, Span};

    use proptest::prelude::*;

    fn finding(category: &str, action: VerdictAction, span: Option<Span>) -> Finding {
        Finding {
            detector: "test".to_owned(),
            category: category.to_owned(),
            severity: Severity::Medium,
            confidence: 0.9,
            span,
            suggested_action: action,
            detector_version: None,
        }
    }

    fn span(start: u32, end: u32) -> Span {
        Span { start, end }
    }

    #[test]
    fn redacts_only_the_matched_span() {
        let text = "Hi, my email is jane.doe@example.com, please get back to me.";
        let start = text
            .find("jane.doe@example.com")
            .expect("fixture contains the email") as u32;
        let end = start + "jane.doe@example.com".len() as u32;
        let findings = vec![finding(
            "pii.email",
            VerdictAction::Redact,
            Some(span(start, end)),
        )];

        let redacted = redact_spans(text, &findings);

        assert_eq!(
            redacted,
            "Hi, my email is [REDACTED], please get back to me."
        );
    }

    #[test]
    fn redacts_multiple_non_overlapping_spans() {
        let text = "email a@b.com and phone 555-1234 both here";
        let email_start = text.find("a@b.com").expect("fixture") as u32;
        let email_end = email_start + "a@b.com".len() as u32;
        let phone_start = text.find("555-1234").expect("fixture") as u32;
        let phone_end = phone_start + "555-1234".len() as u32;

        let findings = vec![
            finding(
                "pii.email",
                VerdictAction::Redact,
                Some(span(email_start, email_end)),
            ),
            finding(
                "pii.phone",
                VerdictAction::Redact,
                Some(span(phone_start, phone_end)),
            ),
        ];

        let redacted = redact_spans(text, &findings);

        assert_eq!(redacted, "email [REDACTED] and phone [REDACTED] both here");
    }

    #[test]
    fn merges_overlapping_spans_into_one_placeholder() {
        let text = "0123456789";
        let findings = vec![
            finding("a", VerdictAction::Redact, Some(span(2, 6))),
            finding("b", VerdictAction::Redact, Some(span(4, 8))),
        ];

        let redacted = redact_spans(text, &findings);

        assert_eq!(redacted, "01[REDACTED]89");
    }

    #[test]
    fn ignores_findings_that_do_not_suggest_redact() {
        let text = "just chatting, nothing sensitive";
        let findings = vec![finding("pii.email", VerdictAction::Warn, Some(span(0, 4)))];

        assert_eq!(redact_spans(text, &findings), text);
    }

    #[test]
    fn falls_back_to_whole_text_when_a_redact_finding_has_no_span() {
        let text = "some text";
        let findings = vec![finding("pii.generic", VerdictAction::Redact, None)];

        assert_eq!(redact_spans(text, &findings), REDACTED_PLACEHOLDER);
    }

    #[test]
    fn falls_back_to_whole_text_when_span_is_out_of_bounds() {
        let text = "short";
        let findings = vec![finding(
            "pii.email",
            VerdictAction::Redact,
            Some(span(0, 100)),
        )];

        assert_eq!(redact_spans(text, &findings), REDACTED_PLACEHOLDER);
    }

    #[test]
    fn no_redact_findings_leaves_text_unchanged() {
        let text = "hello world";
        assert_eq!(redact_spans(text, &[]), text);
    }

    #[test]
    fn falls_back_to_whole_text_when_one_of_several_spans_is_malformed() {
        let text = "secret-one and secret-two";
        let findings = vec![
            finding("a", VerdictAction::Redact, Some(span(0, 10))),
            finding("b", VerdictAction::Redact, Some(span(15, 100))),
        ];

        assert_eq!(redact_spans(text, &findings), REDACTED_PLACEHOLDER);
    }

    #[test]
    fn falls_back_when_a_span_is_inverted_or_splits_a_char() {
        let text = "caf\u{e9} secret";
        let inverted = vec![
            finding("a", VerdictAction::Redact, Some(span(5, 11))),
            finding("b", VerdictAction::Redact, Some(span(3, 1))),
        ];
        assert_eq!(redact_spans(text, &inverted), REDACTED_PLACEHOLDER);

        // U+00E9 is two bytes at 3..5; 4 is inside it.
        let split = vec![finding("a", VerdictAction::Redact, Some(span(4, 11)))];
        assert_eq!(redact_spans(text, &split), REDACTED_PLACEHOLDER);
    }

    #[test]
    fn merges_adjacent_spans() {
        let text = "0123456789";
        let findings = vec![
            finding("a", VerdictAction::Redact, Some(span(2, 4))),
            finding("b", VerdictAction::Redact, Some(span(4, 6))),
        ];

        assert_eq!(redact_spans(text, &findings), "01[REDACTED]6789");
    }

    #[test]
    fn merges_nested_spans() {
        let text = "0123456789";
        let findings = vec![
            finding("outer", VerdictAction::Redact, Some(span(1, 9))),
            finding("inner", VerdictAction::Redact, Some(span(3, 5))),
        ];

        assert_eq!(redact_spans(text, &findings), "0[REDACTED]9");
    }

    #[test]
    fn duplicate_spans_produce_one_placeholder() {
        let text = "0123456789";
        let findings = vec![
            finding("a", VerdictAction::Redact, Some(span(3, 5))),
            finding("b", VerdictAction::Redact, Some(span(3, 5))),
        ];

        assert_eq!(redact_spans(text, &findings), "012[REDACTED]56789");
    }

    #[test]
    fn non_redact_findings_with_bad_spans_are_ignored() {
        let text = "0123456789";
        let findings = vec![
            finding("a", VerdictAction::Redact, Some(span(0, 2))),
            finding("b", VerdictAction::Warn, Some(span(50, 60))),
        ];

        assert_eq!(redact_spans(text, &findings), "[REDACTED]23456789");
    }

    fn arb_findings(len: u32) -> impl Strategy<Value = Vec<Finding>> {
        let bound = len + 4;
        proptest::collection::vec(
            (0..=bound, 0..=bound, any::<bool>()).prop_map(|(start, end, redact)| {
                let action = if redact {
                    VerdictAction::Redact
                } else {
                    VerdictAction::Warn
                };
                finding("p", action, Some(span(start, end)))
            }),
            0..6,
        )
    }

    proptest! {
        /// Never panics, and when the result is not the whole-text
        /// placeholder, no byte covered by a `Redact` span survives: every
        /// byte outside the spans is kept in order and nothing else is.
        #[test]
        fn redaction_never_leaks_a_redact_span(
            (text, findings) in "[a-z\u{e9} ]{0,24}".prop_flat_map(|text| {
                let len = text.len() as u32;
                (Just(text), arb_findings(len))
            })
        ) {
            let out = redact_spans(&text, &findings);
            let redact: Vec<(usize, usize)> = findings
                .iter()
                .filter(|f| f.suggested_action == VerdictAction::Redact)
                .filter_map(|f| f.span)
                .map(|s| (s.start as usize, s.end as usize))
                .collect();

            if redact.is_empty() {
                prop_assert_eq!(out, text);
            } else if out != REDACTED_PLACEHOLDER {
                let kept: String = text
                    .char_indices()
                    .filter(|&(i, _)| !redact.iter().any(|&(s, e)| s <= i && i < e))
                    .map(|(_, c)| c)
                    .collect();
                prop_assert_eq!(out.replace(REDACTED_PLACEHOLDER, ""), kept);
            }
        }
    }
}
