//! Per-tenant detector policy helpers: threshold filtering, category action
//! overrides, and verdict aggregation.
//!
//! # Verdict-aggregation precedence rule
//!
//! The rule below is the one `docs/DESIGN.md` documents. It is the simplest
//! sound rule, with no per-tenant weighting schemes:
//!
//! 1. Findings below the tenant's configured confidence threshold for their
//!    category are dropped (a low-confidence "block" from a noisy detector
//!    should not out-rank nothing at all). A confidence that is not a number
//!    in `[0.0, 1.0]` (a plugin defect) fails closed: it is read as `1.0`, so
//!    the finding always counts.
//! 2. Each remaining finding's `suggested_action` is replaced by the most
//!    specific matching entry in `TenantPolicy.category_action_overrides`, if
//!    any (see `matching_override`). Keys match whole dot-separated segments:
//!    `"pii"` matches `"pii"` and `"pii.email"`, never `"pii_legacy.x"`. Empty
//!    by default — detectors' own suggested actions are authoritative unless
//!    a policy says otherwise.
//! 3. Among the remaining findings that suggest more than `Allow`, the
//!    aggregated action is the **maximum** `VerdictAction` by declaration
//!    order: `Block > Redact > Warn > Audit > Allow`. A single detector
//!    voting `Block` always wins, regardless of how many other detectors vote
//!    lower or how confident they are.
//! 4. If no such finding remains, the tenant's `default_action` applies. An
//!    `Allow` finding never stands in for it, so a backend cannot lower a
//!    fail-closed default by reporting a placeholder.
//!
//! This is intentionally simple; smarter aggregation (confidence-weighted
//! voting, per-category default actions) is left as future work.

use prompt_scanner_sdk::{Finding, TenantPolicy, VerdictAction};

/// `confidence` when it is a number in `[0.0, 1.0]`; otherwise `1.0`, so a
/// defective value fails closed.
fn effective_confidence(confidence: f32) -> f32 {
    if (0.0..=1.0).contains(&confidence) {
        confidence
    } else {
        tracing::warn!(
            confidence = %confidence,
            "scanner backend reported a confidence outside [0, 1]; counting the finding"
        );
        1.0
    }
}

/// The tenant's confidence threshold for `finding`'s category. Categories
/// with no configured threshold always pass (threshold defaults to `0.0`).
/// The boundary passes: a confidence equal to the threshold counts.
fn threshold_for(finding: &Finding, policy: &TenantPolicy) -> f32 {
    policy
        .thresholds
        .get(&finding.category)
        .copied()
        .unwrap_or(0.0)
}

/// Whether override key `key` covers `category`: equal, or a whole leading
/// run of its dot-separated segments.
fn covers(key: &str, category: &str) -> bool {
    !key.is_empty()
        && category
            .strip_prefix(key)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// Returns the most specific key in `policy.category_action_overrides` that
/// covers `category`, if any — e.g. an override keyed `"pii"` applies to
/// `"pii.email"`, `"pii.url"`, etc., and `"pii.email"` wins over `"pii"`.
fn matching_override(category: &str, policy: &TenantPolicy) -> Option<VerdictAction> {
    policy
        .category_action_overrides
        .iter()
        .filter(|(key, _)| covers(key, category))
        .max_by_key(|(key, _)| key.len())
        .map(|(_, action)| *action)
}

/// Applies `policy.category_action_overrides` to `finding.suggested_action`
/// in place, if a matching override is configured.
fn apply_action_override(finding: &mut Finding, policy: &TenantPolicy) {
    if let Some(action) = matching_override(&finding.category, policy) {
        finding.suggested_action = action;
    }
}

/// Aggregate a set of detector findings into a single verdict action, per
/// the precedence rule documented above.
///
/// Returns `(action, findings_considered)` — `findings_considered` is the
/// subset of `findings` that passed the threshold filter (the findings
/// actually recorded against the resulting verdict), with any configured
/// `category_action_overrides` already applied to `suggested_action`.
#[must_use]
pub(crate) fn aggregate_verdict(
    findings: &[Finding],
    policy: &TenantPolicy,
) -> (VerdictAction, Vec<Finding>) {
    let considered: Vec<Finding> = findings
        .iter()
        .filter_map(|f| {
            // Computed once per finding, so a defective value warns once.
            let confidence = effective_confidence(f.confidence);
            (confidence >= threshold_for(f, policy)).then(|| {
                let mut f = f.clone();
                f.confidence = confidence;
                apply_action_override(&mut f, policy);
                f
            })
        })
        .collect();

    let action = considered
        .iter()
        .map(|f| f.suggested_action)
        .filter(|a| *a > VerdictAction::Allow)
        .max()
        .unwrap_or(policy.default_action);

    (action, considered)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use prompt_scanner_sdk::Severity;
    use uuid::Uuid;

    use super::*;

    fn finding(category: &str, confidence: f32, action: VerdictAction) -> Finding {
        Finding {
            detector: "test-detector".to_owned(),
            category: category.to_owned(),
            severity: Severity::Medium,
            confidence,
            span: None,
            suggested_action: action,
            detector_version: None,
        }
    }

    fn policy() -> TenantPolicy {
        let mut p = TenantPolicy::default_for_tenant(Uuid::nil());
        p.thresholds.insert("secret.aws_key".to_owned(), 0.8);
        p
    }

    #[test]
    fn block_wins_over_warn() {
        let findings = vec![
            finding("pii.email", 0.9, VerdictAction::Warn),
            finding("secret.aws_key", 0.95, VerdictAction::Block),
        ];
        let (action, considered) = aggregate_verdict(&findings, &policy());
        assert_eq!(action, VerdictAction::Block);
        assert_eq!(considered.len(), 2);
    }

    #[test]
    fn low_confidence_finding_is_dropped() {
        let findings = vec![finding("secret.aws_key", 0.5, VerdictAction::Block)];
        let (action, considered) = aggregate_verdict(&findings, &policy());
        // Below the 0.8 threshold configured for this category → dropped →
        // falls back to the tenant's default_action (Allow).
        assert_eq!(action, VerdictAction::Allow);
        assert!(considered.is_empty());
    }

    #[test]
    fn no_findings_falls_back_to_default_action() {
        let mut p = policy();
        p.default_action = VerdictAction::Audit;
        let (action, considered) = aggregate_verdict(&[], &p);
        assert_eq!(action, VerdictAction::Audit);
        assert!(considered.is_empty());
    }

    /// `default_action` is a **fallback**, not a floor: it applies when no
    /// finding suggests more than `Allow`. An `Allow` finding (even a
    /// zero-confidence placeholder) never consumes it, so a backend cannot
    /// lower a fail-closed default.
    #[test]
    fn allow_findings_never_replace_a_fail_closed_default_action() {
        let mut p = policy();
        p.default_action = VerdictAction::Block;

        let (empty, considered) = aggregate_verdict(&[], &p);
        assert_eq!(empty, VerdictAction::Block);
        assert!(considered.is_empty());

        let (with_placeholder, considered) =
            aggregate_verdict(&[finding("none", 0.0, VerdictAction::Allow)], &p);
        assert_eq!(with_placeholder, VerdictAction::Block);
        assert_eq!(considered.len(), 1, "the finding is still reported");

        // A stronger finding still wins over a weaker default.
        p.default_action = VerdictAction::Audit;
        let (action, _) = aggregate_verdict(&[finding("x", 0.9, VerdictAction::Warn)], &p);
        assert_eq!(action, VerdictAction::Warn);
    }

    #[test]
    fn threshold_boundary_counts() {
        let p = policy(); // secret.aws_key: 0.8
        let at = aggregate_verdict(&[finding("secret.aws_key", 0.8, VerdictAction::Block)], &p);
        assert_eq!(at.0, VerdictAction::Block, "equal to the threshold passes");
        let below = aggregate_verdict(
            &[finding("secret.aws_key", 0.799_99, VerdictAction::Block)],
            &p,
        );
        assert_eq!(below.0, VerdictAction::Allow, "just below is dropped");
    }

    #[test]
    fn an_invalid_confidence_fails_closed() {
        let p = policy();
        for bad in [f32::NAN, 1.5, -0.1, f32::INFINITY] {
            let (action, considered) =
                aggregate_verdict(&[finding("secret.aws_key", bad, VerdictAction::Block)], &p);
            assert_eq!(action, VerdictAction::Block, "confidence {bad}");
            assert!((considered[0].confidence - 1.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    #[tracing_test::traced_test]
    fn an_invalid_confidence_warns_once_per_finding() {
        let (_, considered) = aggregate_verdict(
            &[finding("secret.aws_key", 1.5, VerdictAction::Block)],
            &policy(),
        );
        assert_eq!(considered.len(), 1);
        logs_assert(|lines: &[&str]| {
            let warnings = lines
                .iter()
                .filter(|l| l.contains("confidence outside [0, 1]"))
                .count();
            if warnings == 1 {
                Ok(())
            } else {
                Err(format!("expected one warning, got {warnings}"))
            }
        });
    }

    #[test]
    fn overrides_match_whole_segments_only() {
        let mut p = policy();
        p.category_action_overrides
            .insert("pii".to_owned(), VerdictAction::Allow);
        let findings = vec![
            finding("pii", 0.9, VerdictAction::Block),
            finding("pii.email", 0.9, VerdictAction::Block),
            finding("pii_legacy.x", 0.9, VerdictAction::Block),
            finding("piis", 0.9, VerdictAction::Block),
        ];
        let (_, considered) = aggregate_verdict(&findings, &p);
        let actions: Vec<_> = considered.iter().map(|f| f.suggested_action).collect();
        assert_eq!(
            actions,
            [
                VerdictAction::Allow,
                VerdictAction::Allow,
                VerdictAction::Block,
                VerdictAction::Block
            ]
        );
    }

    #[test]
    fn category_override_replaces_suggested_action() {
        let mut p = policy();
        p.category_action_overrides
            .insert("pii".to_owned(), VerdictAction::Allow);
        let findings = vec![finding("pii.email", 0.9, VerdictAction::Redact)];
        let (action, considered) = aggregate_verdict(&findings, &p);
        // Overridden to Allow: no actionable finding, so the default applies.
        assert_eq!(action, p.default_action);
        assert_eq!(considered[0].suggested_action, VerdictAction::Allow);
    }

    #[test]
    fn longest_matching_override_prefix_wins() {
        let mut p = policy();
        p.category_action_overrides
            .insert("pii".to_owned(), VerdictAction::Allow);
        p.category_action_overrides
            .insert("pii.email".to_owned(), VerdictAction::Block);
        let findings = vec![
            finding("pii.email", 0.9, VerdictAction::Redact),
            finding("pii.phone", 0.9, VerdictAction::Redact),
        ];
        let (action, considered) = aggregate_verdict(&findings, &p);
        assert_eq!(action, VerdictAction::Block);
        assert_eq!(
            considered
                .iter()
                .find(|f| f.category == "pii.email")
                .unwrap()
                .suggested_action,
            VerdictAction::Block
        );
        assert_eq!(
            considered
                .iter()
                .find(|f| f.category == "pii.phone")
                .unwrap()
                .suggested_action,
            VerdictAction::Allow
        );
    }

    #[test]
    fn no_override_leaves_suggested_action_untouched() {
        let findings = vec![finding("secret.aws_key", 0.95, VerdictAction::Block)];
        let (action, considered) = aggregate_verdict(&findings, &policy());
        assert_eq!(action, VerdictAction::Block);
        assert_eq!(considered[0].suggested_action, VerdictAction::Block);
    }

    #[test]
    fn precedence_ordering_is_block_redact_warn_audit_allow() {
        assert!(VerdictAction::Block > VerdictAction::Redact);
        assert!(VerdictAction::Redact > VerdictAction::Warn);
        assert!(VerdictAction::Warn > VerdictAction::Audit);
        assert!(VerdictAction::Audit > VerdictAction::Allow);
    }
}
