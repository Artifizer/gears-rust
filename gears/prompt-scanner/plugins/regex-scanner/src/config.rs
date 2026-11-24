//! Gear configuration — the pattern set is data, not code.
//!
//! Every detection this backend performs comes from [`PatternConfig`] entries
//! in gear config, compiled once at `init` (see `crate::gear`). The config
//! contract is documented in this crate's `README.md`.
//!
//! # Why `patterns` defaults to a built-in set rather than empty
//!
//! An absent `patterns` key yields [`default_patterns`] — the seventeen
//! patterns this backend shipped with — so a deployment that does not mention
//! patterns keeps detecting. An *explicitly* empty list (`patterns: []`) is
//! honoured and disables detection, per the contract. serde distinguishes the
//! two, and the distinction matters: defaulting to empty would let
//! `config: {}` silently turn a security backend into a no-op.

use std::num::NonZeroUsize;

use prompt_scanner_sdk::VerdictAction;
use serde::{Deserialize, Serialize};

use crate::domain::pattern_set::DEFAULT_MAX_MATCHES;

/// Upper bound accepted for `entropy_threshold`, in bits per character.
///
/// Values above it are almost certainly a unit mistake: a value drawn from the
/// `secret.generic` alphabet (66 symbols) tops out near 6 bits/char, so a
/// threshold this high already disables the validator.
pub const MAX_ENTROPY_THRESHOLD: f64 = 8.0;

/// Configuration for the `regex-scanner` gear.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegexScannerConfig {
    #[serde(default = "default_vendor")]
    pub vendor: String,
    #[serde(default = "default_priority")]
    pub priority: i16,
    /// Minimum Shannon entropy (bits/char) a value must carry for a pattern
    /// using the `entropy` validator to report it. Must be finite and in
    /// `0.0..=`[`MAX_ENTROPY_THRESHOLD`]; checked at `init`.
    #[serde(default = "default_entropy_threshold")]
    pub entropy_threshold: f64,
    /// Most matches reported per scan. Past it the scan stops and the backend
    /// reports one span-less detection carrying the most severe cut action,
    /// so the unreported material is redacted or blocked rather than leaked.
    #[serde(default = "default_max_matches")]
    pub max_matches: NonZeroUsize,
    /// Patterns to run. Absent means [`default_patterns`]; an explicit empty
    /// list disables detection.
    #[serde(default = "default_patterns")]
    pub patterns: Vec<PatternConfig>,
}

/// One detection pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatternConfig {
    /// Reported verbatim as `Finding::category`. Keep it dotted
    /// (`family.thing`) so a tenant's `category_action_overrides` can target a
    /// whole family by prefix.
    pub category: String,
    /// Rust `regex` crate syntax — no backreferences, no lookaround.
    pub regex: String,
    /// Confidence reported on the finding. Defaults to [`Tier::Pattern`].
    #[serde(default)]
    pub tier: Tier,
    /// The finding's `suggested_action`, overridable per tenant.
    pub action: VerdictAction,
    /// Post-match check; a match that fails it is dropped.
    #[serde(default)]
    pub validator: Option<Validator>,
    /// Capture group the validator inspects. The reported span always covers
    /// group 0.
    #[serde(default)]
    pub capture: usize,
}

/// How a match was confirmed, cheapest to most certain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// A bare regex match with no further verification.
    #[default]
    Pattern,
    /// A regex match plus a structural check short of a full checksum.
    Structural,
    /// A regex match confirmed by a checksum or parser.
    Validated,
}

impl Tier {
    /// Numeric confidence reported on the resulting `Finding`.
    #[must_use]
    pub const fn confidence(self) -> f32 {
        match self {
            Self::Pattern => 0.5,
            Self::Structural => 0.75,
            Self::Validated => 0.95,
        }
    }
}

/// Post-match checks a pattern may attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Validator {
    /// Luhn checksum over the digits of the match, which must number 13-19
    /// once separators are stripped (the PAN lengths the major networks use).
    Luhn,
    /// IBAN mod-97-10 (ISO 7064), whitespace stripped first.
    Iban,
    /// Parses as an IPv4 address.
    Ipv4,
    /// Parses as an IPv6 address.
    Ipv6,
    /// Three base64url segments whose header decodes to something JSON-shaped.
    /// A shape check, not signature verification.
    Jwt,
    /// Shannon entropy at or above the configured `entropy_threshold`.
    Entropy,
}

fn default_vendor() -> String {
    "cf.builtin.regex_scanner".to_owned()
}

const fn default_priority() -> i16 {
    500
}

const fn default_entropy_threshold() -> f64 {
    3.0
}

const fn default_max_matches() -> NonZeroUsize {
    DEFAULT_MAX_MATCHES
}

impl Default for RegexScannerConfig {
    fn default() -> Self {
        Self {
            vendor: default_vendor(),
            priority: default_priority(),
            entropy_threshold: default_entropy_threshold(),
            max_matches: default_max_matches(),
            patterns: default_patterns(),
        }
    }
}

/// Terse constructor for the built-in table below.
fn p(
    category: &str,
    regex: &str,
    tier: Tier,
    action: VerdictAction,
    validator: Option<Validator>,
    capture: usize,
) -> PatternConfig {
    PatternConfig {
        category: category.to_owned(),
        regex: regex.to_owned(),
        tier,
        action,
        validator,
        capture,
    }
}

/// The patterns this backend detects when config names none.
///
/// The same categories, tiers and actions the gear shipped with. Two regexes
/// were widened since: IPv6 now also matches forms that start with `::` or
/// end in an embedded IPv4 address, and IBAN now also matches the printed
/// form with spaces between groups of four.
#[must_use]
pub fn default_patterns() -> Vec<PatternConfig> {
    use Tier::{Pattern, Structural, Validated};
    use Validator::{Entropy, Iban, Ipv4, Ipv6, Jwt, Luhn};
    use VerdictAction::{Block, Redact, Warn};

    vec![
        // -- contact ---------------------------------------------------------
        p(
            "pii.email",
            r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
            Pattern,
            Redact,
            None,
            0,
        ),
        p(
            "pii.url",
            r#"https?://[^\s<>"']+"#,
            Pattern,
            Redact,
            None,
            0,
        ),
        p(
            "pii.ip_address",
            r"\b(?:\d{1,3}\.){3}\d{1,3}\b",
            Validated,
            Redact,
            Some(Ipv4),
            0,
        ),
        p(
            "pii.ip_address",
            // Half word boundaries, not `\b`: `\b` before a leading `:` needs a
            // word character in front, so `::1` could never match. The IPv4
            // tail is tried first so `::ffff:10.0.0.1` is not cut at `10`.
            r"\b{start-half}(?:[A-Fa-f0-9]{0,4}:){2,7}(?:(?:\d{1,3}\.){3}\d{1,3}|[A-Fa-f0-9]{1,4})?\b{end-half}",
            Validated,
            Redact,
            Some(Ipv6),
            0,
        ),
        // Pattern-tier on purpose: no checksum exists for phone numbers and
        // the shape over-matches generic digit runs.
        p(
            "pii.phone",
            r"\+?\d{1,3}?[\s.-]?\(?\d{2,4}\)?[\s.-]?\d{3,4}[\s.-]?\d{3,4}",
            Pattern,
            Redact,
            None,
            0,
        ),
        // -- financial -------------------------------------------------------
        // A Luhn-valid PAN is high-confidence secret material, not merely PII.
        p(
            "financial.credit_card",
            r"\b(?:\d[ -]?){12,18}\d\b",
            Validated,
            Block,
            Some(Luhn),
            0,
        ),
        p(
            "financial.iban",
            // Groups of four with an optional single space between them, so the
            // printed form (`DE89 3704 0044 0532 0130 00`) matches as one span;
            // the validator strips the spaces. Fixed-width groups keep a
            // following capitalised word from being glued onto the number.
            r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b",
            Validated,
            Redact,
            Some(Iban),
            0,
        ),
        // -- vendor credentials ----------------------------------------------
        p(
            "secret.aws_key",
            r"\bAKIA[0-9A-Z]{16}\b",
            Structural,
            Block,
            None,
            0,
        ),
        p(
            "secret.github_token",
            r"\bgh[pousr]_[A-Za-z0-9]{36}\b",
            Structural,
            Block,
            None,
            0,
        ),
        p(
            "secret.openai_key",
            r"\bsk-[A-Za-z0-9]{20,}\b",
            Structural,
            Block,
            None,
            0,
        ),
        p(
            "secret.anthropic_key",
            r"\bsk-ant-[A-Za-z0-9_-]{20,}\b",
            Structural,
            Block,
            None,
            0,
        ),
        p(
            "secret.slack_token",
            r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
            Structural,
            Block,
            None,
            0,
        ),
        p(
            "secret.stripe_key",
            r"\bsk_live_[A-Za-z0-9]{24,}\b",
            Structural,
            Block,
            None,
            0,
        ),
        // -- secret shapes ---------------------------------------------------
        p(
            "secret.jwt",
            r"\b[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
            Structural,
            Block,
            Some(Jwt),
            0,
        ),
        p(
            "secret.private_key",
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----",
            Structural,
            Block,
            None,
            0,
        ),
        p(
            "secret.connection_string",
            r"\b[A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]+:[^\s:/@]+@[^\s/]+",
            Pattern,
            Block,
            None,
            0,
        ),
        // `capture: 1` — the validator inspects the value, the span covers the
        // whole `key=value` match.
        p(
            "secret.generic",
            r#"(?i)\b(?:password|passwd|secret|api[_-]?key|access[_-]?token|token)\s*[:=]\s*["']?([A-Za-z0-9+/_.-]{8,})["']?"#,
            Pattern,
            Warn,
            Some(Entropy),
            1,
        ),
    ]
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn parse(json: &str) -> RegexScannerConfig {
        serde_json::from_str(json).expect("valid config")
    }

    #[test]
    fn absent_patterns_key_means_the_builtin_set() {
        let cfg = parse("{}");
        assert_eq!(cfg.patterns.len(), default_patterns().len());
        assert!(!cfg.patterns.is_empty());
    }

    #[test]
    fn an_explicitly_empty_patterns_list_is_honoured() {
        let cfg = parse(r#"{"patterns": []}"#);
        assert!(cfg.patterns.is_empty());
    }

    #[test]
    fn defaults_are_pinned() {
        let cfg = parse("{}");
        assert!((cfg.entropy_threshold - 3.0).abs() < f64::EPSILON);
        assert_eq!(cfg.max_matches, DEFAULT_MAX_MATCHES);
        assert_eq!(cfg.max_matches.get(), 1024);
        assert_eq!(cfg.vendor, "cf.builtin.regex_scanner");
        assert_eq!(cfg.priority, 500);

        let dflt = RegexScannerConfig::default();
        assert!((dflt.entropy_threshold - cfg.entropy_threshold).abs() < f64::EPSILON);
        assert_eq!(dflt.max_matches, cfg.max_matches);
        assert_eq!(dflt.patterns.len(), cfg.patterns.len());
    }

    #[test]
    fn zero_max_matches_is_rejected() {
        assert!(serde_json::from_str::<RegexScannerConfig>(r#"{"max_matches": 0}"#).is_err());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(serde_json::from_str::<RegexScannerConfig>(r#"{"patern": []}"#).is_err());
    }

    #[test]
    fn a_pattern_entry_defaults_tier_validator_and_capture() {
        let cfg = parse(r#"{"patterns": [{"category": "x.y", "regex": "z", "action": "warn"}]}"#);
        let p = &cfg.patterns[0];
        assert_eq!(p.tier, Tier::Pattern);
        assert_eq!(p.validator, None);
        assert_eq!(p.capture, 0);
        assert_eq!(p.action, VerdictAction::Warn);
    }
}
