//! Compiled pattern set — the whole of this backend's detection.
//!
//! [`PatternSet::compile`] turns [`PatternConfig`] entries into regexes once,
//! at gear init, so an invalid pattern is a startup failure rather than a
//! per-request one. [`PatternSet::scan`] then runs them over a payload.
//!
//! # One pass, not one pass per pattern
//!
//! The obvious implementation runs each regex over the payload in turn: N
//! patterns means N scans of the text. Instead the compiled set keeps a
//! [`RegexSet`] alongside the individual regexes. `RegexSet` compiles every
//! pattern into a single automaton, so `matches()` answers "which of these
//! seventeen patterns occur?" in **one** pass, and only the patterns it names
//! are then re-run to extract spans.
//!
//! The common case for a scanning proxy is a prompt containing no secrets at
//! all, and that case collapses from seventeen passes to one. A payload with a
//! single email costs one pass plus one. `RegexSet` cannot report capture
//! groups or offsets — hence the second pass — but it does not need to.
//!
//! # Bounded output
//!
//! A hostile payload can hold millions of matches. [`PatternSet::scan`] stops
//! collecting at the configured match limit and says so in
//! [`ScanOutput::incomplete`], so the caller can fail safe instead of
//! silently passing the unreported matches through.

use std::collections::HashMap;
use std::num::NonZeroUsize;

use prompt_scanner_sdk::{Span, VerdictAction};
use regex::{Regex, RegexSet, RegexSetBuilder};

use crate::config::{MAX_ENTROPY_THRESHOLD, PatternConfig, Tier, Validator};
use crate::domain::validators;

/// Compiled-size limit of the combined [`RegexSet`] — the `regex` crate's
/// own default, spelled out so tests can lower it.
const SET_SIZE_LIMIT: usize = 10 * (1 << 20);

/// Default cap on matches collected per scan.
// `NonZeroUsize::MIN` (1) + 1023: a const-safe way to spell 1024.
pub const DEFAULT_MAX_MATCHES: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1023);

/// A single match against the scanned payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub category: String,
    /// Byte offsets of capture group 0, always — even when a validator
    /// inspected a different group.
    pub span: Span,
    pub tier: Tier,
    pub suggested_action: VerdictAction,
}

/// Result of [`PatternSet::scan`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanOutput {
    /// Every reported match, at most the configured match limit.
    pub matches: Vec<Match>,
    /// `Some(action)` when some matches were not reported — the match limit
    /// was hit, or a span did not fit the SDK's `u32` offsets. `action` is the
    /// most severe action among the patterns whose matches were cut, so the
    /// caller can fail safe for exactly what it could not locate.
    pub incomplete: Option<VerdictAction>,
}

impl ScanOutput {
    fn mark_incomplete(&mut self, action: VerdictAction) {
        self.incomplete = Some(self.incomplete.map_or(action, |prev| prev.max(action)));
    }
}

/// Why a pattern set could not be compiled. Surfaced from `Gear::init`, so the
/// message names the offending pattern rather than just the regex error.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("pattern {index} (category `{category}`) has an invalid regex: {source}")]
    InvalidRegex {
        index: usize,
        category: String,
        #[source]
        source: Box<regex::Error>,
    },
    #[error(
        "pattern {index} (category `{category}`) names capture group {capture}, but its regex has \
         only {available} group(s)"
    )]
    MissingCaptureGroup {
        index: usize,
        category: String,
        capture: usize,
        available: usize,
    },
    #[error("pattern {index} (category `{category}`) has an empty category")]
    EmptyCategory { index: usize, category: String },
    /// Every regex compiled on its own, but the combined [`RegexSet`] did not
    /// (in practice: it exceeds the regex size limit).
    #[error("the combined pattern set ({patterns} patterns) failed to compile: {source}")]
    PatternSet {
        patterns: usize,
        #[source]
        source: Box<regex::Error>,
    },
    #[error(
        "entropy_threshold must be a finite number in 0.0..={MAX_ENTROPY_THRESHOLD}, got {value}"
    )]
    InvalidEntropyThreshold { value: f64 },
}

#[derive(Debug)]
struct Compiled {
    category: String,
    regex: Regex,
    tier: Tier,
    action: VerdictAction,
    validator: Option<Validator>,
    capture: usize,
}

/// Compiled, ready-to-run detection patterns.
#[derive(Debug)]
pub struct PatternSet {
    /// Single-automaton prefilter over every pattern; indices line up with
    /// `patterns`.
    set: RegexSet,
    patterns: Vec<Compiled>,
    entropy_threshold: f64,
    max_matches: NonZeroUsize,
}

impl PatternSet {
    /// Compile `patterns`. Returns the first error rather than skipping bad
    /// entries: a silently dropped pattern is a detector that stops detecting
    /// without telling anyone.
    ///
    /// The match limit starts at [`DEFAULT_MAX_MATCHES`]; change it with
    /// [`Self::with_max_matches`].
    ///
    /// # Errors
    ///
    /// [`CompileError`] when `entropy_threshold` is not finite or out of
    /// range, a regex does not parse, a pattern names a capture group its
    /// regex does not have, a category is empty, or the combined set is too
    /// large.
    pub fn compile(
        patterns: &[PatternConfig],
        entropy_threshold: f64,
    ) -> Result<Self, CompileError> {
        Self::compile_with_set_size_limit(patterns, entropy_threshold, SET_SIZE_LIMIT)
    }

    fn compile_with_set_size_limit(
        patterns: &[PatternConfig],
        entropy_threshold: f64,
        set_size_limit: usize,
    ) -> Result<Self, CompileError> {
        if !(0.0..=MAX_ENTROPY_THRESHOLD).contains(&entropy_threshold) {
            // `contains` is false for NaN, so this also rejects NaN and ±inf.
            return Err(CompileError::InvalidEntropyThreshold {
                value: entropy_threshold,
            });
        }

        let mut compiled = Vec::with_capacity(patterns.len());

        for (index, cfg) in patterns.iter().enumerate() {
            if cfg.category.trim().is_empty() {
                return Err(CompileError::EmptyCategory {
                    index,
                    category: cfg.category.clone(),
                });
            }

            let regex = Regex::new(&cfg.regex).map_err(|source| CompileError::InvalidRegex {
                index,
                category: cfg.category.clone(),
                source: Box::new(source),
            })?;

            // `captures_len` counts group 0, so group N needs captures_len > N.
            let available = regex.captures_len().saturating_sub(1);
            if cfg.capture > available {
                return Err(CompileError::MissingCaptureGroup {
                    index,
                    category: cfg.category.clone(),
                    capture: cfg.capture,
                    available,
                });
            }

            compiled.push(Compiled {
                category: cfg.category.clone(),
                regex,
                tier: cfg.tier,
                action: cfg.action,
                validator: cfg.validator,
                capture: cfg.capture,
            });
        }

        // Every regex here already parsed individually, so the set cannot fail
        // to compile for a syntax reason; it can still exceed the size limit,
        // which is a real configuration error worth reporting as one.
        let set = RegexSetBuilder::new(patterns.iter().map(|c| &c.regex))
            .size_limit(set_size_limit)
            .build()
            .map_err(|source| CompileError::PatternSet {
                patterns: patterns.len(),
                source: Box::new(source),
            })?;

        Ok(Self {
            set,
            patterns: compiled,
            entropy_threshold,
            max_matches: DEFAULT_MAX_MATCHES,
        })
    }

    /// Set the cap on matches collected per scan.
    #[must_use]
    pub const fn with_max_matches(mut self, max_matches: NonZeroUsize) -> Self {
        self.max_matches = max_matches;
        self
    }

    /// Number of compiled patterns.
    #[must_use]
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    /// Whether the set detects nothing — a valid, explicitly configured state.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Every match every pattern finds, each reported separately, up to the
    /// match limit. Overlaps are kept: one payload can hold several distinct
    /// pieces of sensitive material, and the gateway merges spans only at
    /// redaction time.
    ///
    /// CPU-bound: callers on an async runtime should run large payloads on a
    /// blocking thread.
    #[must_use]
    pub fn scan(&self, text: &str) -> ScanOutput {
        let mut out = ScanOutput::default();
        if self.patterns.is_empty() {
            return out;
        }

        // One pass over `text` for all patterns; only the hits are re-run.
        for index in self.set.matches(text) {
            let Some(pattern) = self.patterns.get(index) else {
                continue;
            };
            self.collect_pattern_matches(pattern, text, &mut out);
        }
        out
    }

    fn collect_pattern_matches(&self, pattern: &Compiled, text: &str, out: &mut ScanOutput) {
        // `find_iter` skips capture bookkeeping, so use it whenever the
        // validator inspects group 0 (or there is no validator at all).
        if pattern.capture == 0 {
            for m in pattern.regex.find_iter(text) {
                if self.accepts(pattern, m.as_str()) && !self.push(pattern, m.start(), m.end(), out)
                {
                    return;
                }
            }
            return;
        }

        for caps in pattern.regex.captures_iter(text) {
            let (Some(whole), Some(subject)) = (caps.get(0), caps.get(pattern.capture)) else {
                continue;
            };
            if self.accepts(pattern, subject.as_str())
                && !self.push(pattern, whole.start(), whole.end(), out)
            {
                return;
            }
        }
    }

    /// Record one accepted match. Returns `false` once the match limit is
    /// reached, telling the caller to stop scanning this pattern.
    fn push(&self, pattern: &Compiled, start: usize, end: usize, out: &mut ScanOutput) -> bool {
        if out.matches.len() >= self.max_matches.get() {
            out.mark_incomplete(pattern.action);
            return false;
        }
        match (u32::try_from(start), u32::try_from(end)) {
            (Ok(start), Ok(end)) => out.matches.push(Match {
                category: pattern.category.clone(),
                span: Span { start, end },
                tier: pattern.tier,
                suggested_action: pattern.action,
            }),
            // Unreachable for SDK-bounded payloads (`MAX_PAYLOAD_BYTES` is far
            // below `u32::MAX`); never truncate an offset into a wrong span.
            _ => out.mark_incomplete(pattern.action),
        }
        true
    }

    fn accepts(&self, pattern: &Compiled, candidate: &str) -> bool {
        let Some(validator) = pattern.validator else {
            return true;
        };
        match validator {
            Validator::Luhn => {
                let digits: String = candidate.chars().filter(char::is_ascii_digit).collect();
                (13..=19).contains(&digits.len()) && validators::luhn_check(&digits)
            }
            Validator::Iban => {
                let normalized: String = candidate.chars().filter(|c| !c.is_whitespace()).collect();
                validators::iban_checksum_valid(&normalized)
            }
            Validator::Ipv4 => validators::is_valid_ipv4(candidate),
            Validator::Ipv6 => validators::is_valid_ipv6(candidate),
            Validator::Jwt => validators::looks_like_jwt(candidate),
            Validator::Entropy => validators::shannon_entropy(candidate) >= self.entropy_threshold,
        }
    }

    /// Count of patterns per category, for the startup log.
    #[must_use]
    pub fn categories(&self) -> HashMap<&str, usize> {
        let mut counts = HashMap::new();
        for pattern in &self.patterns {
            *counts.entry(pattern.category.as_str()).or_insert(0) += 1;
        }
        counts
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::config::default_patterns;

    fn builtin() -> PatternSet {
        PatternSet::compile(&default_patterns(), 3.0).expect("built-in patterns must compile")
    }

    fn matches(text: &str) -> Vec<Match> {
        let out = builtin().scan(text);
        assert!(
            out.incomplete.is_none(),
            "unexpected truncation for {text:?}"
        );
        out.matches
    }

    fn categories(text: &str) -> Vec<String> {
        let mut c: Vec<String> = matches(text).into_iter().map(|m| m.category).collect();
        c.sort();
        c.dedup();
        c
    }

    fn slice(text: &str, span: Span) -> &str {
        &text[span.start as usize..span.end as usize]
    }

    fn pattern(category: &str, regex: &str, action: VerdictAction) -> PatternConfig {
        PatternConfig {
            category: category.to_owned(),
            regex: regex.to_owned(),
            tier: Tier::Pattern,
            action,
            validator: None,
            capture: 0,
        }
    }

    // ── every built-in pattern: one positive, one near miss ─────────────────

    const JWT: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.\
                       dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";

    /// `(category, text that must hit it, matched substring, near miss that must not)`.
    const BUILTIN_CASES: &[(&str, &str, &str, &str)] = &[
        (
            "pii.email",
            "contact leaked@example.com now",
            "leaked@example.com",
            "contact leaked@example now",
        ),
        (
            "pii.url",
            "see https://example.com/path?q=1 now",
            "https://example.com/path?q=1",
            "see example.com/path now",
        ),
        (
            "pii.ip_address",
            "server at 192.168.1.1 responded",
            "192.168.1.1",
            "version 999.999.999.999 is invalid",
        ),
        (
            "pii.ip_address",
            "loopback ::1 answered",
            "::1",
            "meet at 12:30:45 sharp",
        ),
        (
            "pii.phone",
            "call me at +1 555-123-4567 today",
            "+1 555-123-4567",
            "call extension 555-1234 today",
        ),
        (
            "financial.credit_card",
            "card 4111 1111 1111 1111 on file",
            "4111 1111 1111 1111",
            "card 4111 1111 1111 1112 on file",
        ),
        (
            "financial.iban",
            "pay to GB82WEST12345698765432 today",
            "GB82WEST12345698765432",
            "pay to GB82WEST12345698765431 today",
        ),
        (
            "secret.aws_key",
            "key is AKIAABCDEFGHIJKLMNOP ok",
            "AKIAABCDEFGHIJKLMNOP",
            "key is AKIAABCDEFGHIJKLMNO ok",
        ),
        (
            "secret.github_token",
            "token ghp_abcdefghijklmnopqrstuvwxyz0123456789 ok",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "token ghx_abcdefghijklmnopqrstuvwxyz0123456789 ok",
        ),
        (
            "secret.openai_key",
            "key sk-abcdefghijklmnopqrstuvwx ok",
            "sk-abcdefghijklmnopqrstuvwx",
            "key sk-abcdefghij ok",
        ),
        (
            "secret.anthropic_key",
            "key sk-ant-api03-abcdefghijklmnopqrst ok",
            "sk-ant-api03-abcdefghijklmnopqrst",
            "key sk-ant-short ok",
        ),
        (
            "secret.slack_token",
            "slack xoxb-1234567890-abcdef ok",
            "xoxb-1234567890-abcdef",
            "slack xoxz-1234567890-abcdef ok",
        ),
        (
            "secret.stripe_key",
            // Split so the source holds no key-shaped literal: secret scanners
            // (GitHub push protection) flag fake fixtures as real Stripe keys.
            concat!("stripe sk_", "live_abcdefghijklmnopqrstuvwx ok"),
            concat!("sk_", "live_abcdefghijklmnopqrstuvwx"),
            concat!("stripe sk_", "test_abcdefghijklmnopqrstuvwx ok"),
        ),
        (
            "secret.jwt",
            JWT,
            JWT,
            "aaaaaaaaaaaa.bbbbbbbbbbbb.cccccccccccc",
        ),
        (
            "secret.private_key",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAK\n-----END RSA PRIVATE KEY-----",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIBOgIBAAJBAK\n-----END RSA PRIVATE KEY-----",
            "-----BEGIN PUBLIC KEY-----\nMIIBOgIBAAJBAK\n-----END PUBLIC KEY-----",
        ),
        (
            "secret.connection_string",
            "dsn postgres://admin:hunter2@db.internal:5432/app ok",
            // The span stops before the path: it covers the credentials.
            "postgres://admin:hunter2@db.internal:5432",
            "dsn postgres://db.internal:5432/app ok",
        ),
        (
            "secret.generic",
            "api_key = s3cr3tV4lueWithEntropy99",
            "api_key = s3cr3tV4lueWithEntropy99",
            "password = changeme",
        ),
    ];

    #[test]
    fn every_builtin_pattern_hits_its_positive_with_the_exact_span() {
        for &(category, text, expected, _) in BUILTIN_CASES {
            let hits: Vec<&str> = matches(text)
                .iter()
                .filter(|m| m.category == category)
                .map(|m| slice(text, m.span))
                .collect();
            assert!(
                hits.contains(&expected),
                "{category}: expected a match on {expected:?} in {text:?}, got {hits:?}"
            );
        }
    }

    #[test]
    fn every_builtin_pattern_ignores_its_near_miss() {
        for &(category, _, _, near_miss) in BUILTIN_CASES {
            assert!(
                !categories(near_miss).contains(&category.to_owned()),
                "{category}: near miss {near_miss:?} must not match"
            );
        }
    }

    #[test]
    fn the_case_table_covers_every_builtin_category() {
        let mut builtin: Vec<String> = default_patterns().into_iter().map(|p| p.category).collect();
        builtin.sort();
        builtin.dedup();
        let mut covered: Vec<String> = BUILTIN_CASES.iter().map(|c| c.0.to_owned()).collect();
        covered.sort();
        covered.dedup();
        assert_eq!(builtin, covered);
    }

    #[test]
    fn ipv6_forms_starting_with_a_colon_or_embedding_ipv4_are_detected() {
        for (text, expected) in [
            ("loopback ::1 answered", "::1"),
            ("mapped ::ffff:10.0.0.1 answered", "::ffff:10.0.0.1"),
            ("host 2001:db8::1 answered", "2001:db8::1"),
            (
                "link-local fe80::1ff:fe23:4567:890a here",
                "fe80::1ff:fe23:4567:890a",
            ),
            (
                "full 2001:0db8:85a3:0000:0000:8a2e:0370:7334 here",
                "2001:0db8:85a3:0000:0000:8a2e:0370:7334",
            ),
        ] {
            let hits: Vec<&str> = matches(text)
                .iter()
                .filter(|m| m.category == "pii.ip_address")
                .map(|m| slice(text, m.span))
                .collect();
            assert!(hits.contains(&expected), "{text:?}: got {hits:?}");
        }
    }

    #[test]
    fn colon_separated_non_addresses_are_not_ipv6() {
        for text in ["mac 00:1A:2B:3C:4D:5E here", "at 12:30:45", "ratio 1:2:3"] {
            assert!(
                !categories(text).contains(&"pii.ip_address".to_owned()),
                "{text:?}"
            );
        }
    }

    #[test]
    fn a_spaced_iban_is_detected_as_one_span() {
        let text = "pay DE89 3704 0044 0532 0130 00 now";
        let hits: Vec<&str> = matches(text)
            .iter()
            .filter(|m| m.category == "financial.iban")
            .map(|m| slice(text, m.span))
            .collect();
        assert_eq!(hits, vec!["DE89 3704 0044 0532 0130 00"]);
    }

    #[test]
    fn clean_text_yields_nothing() {
        assert_eq!(
            builtin().scan("nothing sensitive here"),
            ScanOutput::default()
        );
    }

    #[test]
    fn several_distinct_findings_from_one_payload() {
        let found = categories("mail leaked@example.com and key AKIAABCDEFGHIJKLMNOP");
        assert!(found.contains(&"pii.email".to_owned()));
        assert!(found.contains(&"secret.aws_key".to_owned()));
    }

    #[test]
    fn several_matches_of_one_pattern_get_distinct_spans() {
        let text = "a@example.com, b@example.org and c@example.net";
        let spans: Vec<&str> = matches(text)
            .iter()
            .filter(|m| m.category == "pii.email")
            .map(|m| slice(text, m.span))
            .collect();
        assert_eq!(
            spans,
            vec!["a@example.com", "b@example.org", "c@example.net"]
        );
    }

    // ── tier, span and validator plumbing ───────────────────────────────────

    #[test]
    fn tier_determines_reported_confidence() {
        assert!((Tier::Pattern.confidence() - 0.5).abs() < f32::EPSILON);
        assert!((Tier::Structural.confidence() - 0.75).abs() < f32::EPSILON);
        assert!((Tier::Validated.confidence() - 0.95).abs() < f32::EPSILON);

        let m = matches("server at 192.168.1.1 responded")
            .into_iter()
            .find(|m| m.category == "pii.ip_address")
            .expect("ipv4 match");
        assert_eq!(
            m.tier,
            Tier::Validated,
            "a checksum-validated match is Validated"
        );
    }

    #[test]
    fn span_covers_group_zero_even_when_the_validator_reads_another_group() {
        // `secret.generic` validates capture 1 (the value) but must report the
        // whole `key=value` match.
        let text = "api_key = s3cr3tV4lueWithEntropy99";
        let m = matches(text)
            .into_iter()
            .find(|m| m.category == "secret.generic")
            .expect("generic secret match");
        let matched = slice(text, m.span);
        assert!(
            matched.starts_with("api_key"),
            "span must cover group 0, got {matched:?}"
        );
    }

    #[test]
    fn entropy_threshold_is_honoured() {
        let low = "password = changeme";
        let patterns = default_patterns();

        let permissive = PatternSet::compile(&patterns, 0.0).expect("compile");
        assert!(
            permissive
                .scan(low)
                .matches
                .iter()
                .any(|m| m.category == "secret.generic"),
            "a zero threshold accepts a low-entropy value"
        );

        let strict = PatternSet::compile(&patterns, 6.0).expect("compile");
        assert!(
            !strict
                .scan(low)
                .matches
                .iter()
                .any(|m| m.category == "secret.generic"),
            "a high threshold rejects it"
        );
    }

    // ── bounded output ──────────────────────────────────────────────────────

    #[test]
    fn the_match_limit_caps_output_and_reports_incompleteness() {
        let set = PatternSet::compile(
            &[pattern("t.word", r"\bword\b", VerdictAction::Redact)],
            3.0,
        )
        .expect("compile")
        .with_max_matches(NonZeroUsize::new(2).expect("non-zero"));
        let out = set.scan("word word word word word");
        assert_eq!(out.matches.len(), 2);
        assert_eq!(out.incomplete, Some(VerdictAction::Redact));
    }

    #[test]
    fn incompleteness_carries_the_most_severe_cut_action() {
        let set = PatternSet::compile(
            &[
                pattern("t.a", r"\ba\b", VerdictAction::Warn),
                pattern("t.b", r"\bb\b", VerdictAction::Block),
            ],
            3.0,
        )
        .expect("compile")
        .with_max_matches(NonZeroUsize::MIN);
        let out = set.scan("a a b b");
        assert_eq!(out.matches.len(), 1);
        assert_eq!(out.incomplete, Some(VerdictAction::Block));
    }

    #[test]
    fn exactly_the_limit_is_not_incomplete() {
        let set = PatternSet::compile(&[pattern("t.word", r"\bword\b", VerdictAction::Warn)], 3.0)
            .expect("compile")
            .with_max_matches(NonZeroUsize::new(3).expect("non-zero"));
        let out = set.scan("word word word");
        assert_eq!(out.matches.len(), 3);
        assert_eq!(out.incomplete, None);
    }

    // ── compilation is a startup gate ───────────────────────────────────────

    #[test]
    fn an_out_of_range_entropy_threshold_fails_compilation() {
        for bad in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.1,
            MAX_ENTROPY_THRESHOLD + 0.1,
        ] {
            assert!(
                matches!(
                    PatternSet::compile(&default_patterns(), bad),
                    Err(CompileError::InvalidEntropyThreshold { .. })
                ),
                "threshold {bad} must be rejected"
            );
        }
        for good in [0.0, 3.0, MAX_ENTROPY_THRESHOLD] {
            assert!(
                PatternSet::compile(&default_patterns(), good).is_ok(),
                "{good}"
            );
        }
    }

    #[test]
    fn an_invalid_regex_fails_compilation_and_names_the_pattern() {
        let bad = vec![pattern("broken.thing", "((unclosed", VerdictAction::Warn)];
        let err = PatternSet::compile(&bad, 3.0).expect_err("must not compile");
        let msg = err.to_string();
        assert!(
            msg.contains("broken.thing"),
            "error names the pattern: {msg}"
        );
    }

    #[test]
    fn an_oversized_combined_set_is_a_pattern_set_error() {
        // Each regex compiles on its own (default per-regex limit); the
        // combined set exceeds its (here lowered) size limit.
        let many: Vec<PatternConfig> = (0..4)
            .map(|i| pattern(&format!("big.{i}"), r"\w{20}", VerdictAction::Warn))
            .collect();
        match PatternSet::compile_with_set_size_limit(&many, 3.0, 1024) {
            Err(CompileError::PatternSet { patterns: 4, .. }) => {}
            other => panic!("expected CompileError::PatternSet, got {other:?}"),
        }
    }

    #[test]
    fn a_capture_group_the_regex_lacks_fails_compilation() {
        let bad = vec![PatternConfig {
            category: "broken.capture".to_owned(),
            regex: r"\bAKIA[0-9A-Z]{16}\b".to_owned(),
            tier: Tier::Pattern,
            action: VerdictAction::Warn,
            validator: Some(Validator::Entropy),
            capture: 2,
        }];
        let err = PatternSet::compile(&bad, 3.0).expect_err("must not compile");
        assert!(
            matches!(err, CompileError::MissingCaptureGroup { capture: 2, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn an_empty_category_fails_compilation() {
        let bad = vec![pattern("  ", "x", VerdictAction::Warn)];
        assert!(matches!(
            PatternSet::compile(&bad, 3.0),
            Err(CompileError::EmptyCategory { .. })
        ));
    }

    #[test]
    fn an_explicitly_empty_pattern_set_is_valid_and_detects_nothing() {
        let empty = PatternSet::compile(&[], 3.0).expect("an empty set is valid");
        assert!(empty.is_empty());
        assert_eq!(
            empty.scan("card 4111111111111111 leaked@example.com"),
            ScanOutput::default()
        );
    }

    #[test]
    fn a_custom_pattern_set_replaces_the_built_ins() {
        let custom = vec![PatternConfig {
            category: "custom.ticket".to_owned(),
            regex: r"\bTICKET-\d{4}\b".to_owned(),
            tier: Tier::Structural,
            action: VerdictAction::Audit,
            validator: None,
            capture: 0,
        }];
        let set = PatternSet::compile(&custom, 3.0).expect("compile");
        let found = set.scan("see TICKET-1234 and mail a@b.com").matches;

        assert_eq!(found.len(), 1, "only the configured pattern runs");
        assert_eq!(found[0].category, "custom.ticket");
        assert_eq!(found[0].tier, Tier::Structural);
        assert_eq!(found[0].suggested_action, VerdictAction::Audit);
    }
}
