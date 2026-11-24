use chrono::{TimeDelta, Utc};
use prompt_scanner_sdk::{ContentKind, Severity, VerdictAction};
use uuid::Uuid;

use super::*;
use crate::infra::storage::migrations::m20260805_000001_initial::{POSTGRES, SQLITE};

/// Every variant; the `match` breaks the build when one is added, so this
/// list (and the migration's `CHECK`s) cannot silently fall behind.
fn all_actions() -> [VerdictAction; 5] {
    let all = [
        VerdictAction::Allow,
        VerdictAction::Audit,
        VerdictAction::Warn,
        VerdictAction::Redact,
        VerdictAction::Block,
    ];
    for a in all {
        match a {
            VerdictAction::Allow
            | VerdictAction::Audit
            | VerdictAction::Warn
            | VerdictAction::Redact
            | VerdictAction::Block => {}
        }
    }
    all
}

fn all_severities() -> [Severity; 4] {
    let all = [
        Severity::Low,
        Severity::Medium,
        Severity::High,
        Severity::Critical,
    ];
    for s in all {
        match s {
            Severity::Low | Severity::Medium | Severity::High | Severity::Critical => {}
        }
    }
    all
}

fn all_kinds() -> [ContentKind; 5] {
    for k in ContentKind::ALL {
        match k {
            ContentKind::Prompt
            | ContentKind::Response
            | ContentKind::Upload
            | ContentKind::McpToolCall
            | ContentKind::McpToolResult => {}
        }
    }
    ContentKind::ALL
}

/// The `CHECK (<column> IN (...))` list of `constraint` in `sql`.
fn check_list(sql: &str, constraint: &str) -> String {
    let at = sql.find(constraint).expect("constraint present");
    let rest = &sql[at..];
    let end = rest.find(')').expect("closing paren");
    rest[..end].to_owned()
}

#[test]
fn every_enum_tag_is_allowed_by_both_migrations() {
    for sql in [POSTGRES, SQLITE] {
        let kinds = check_list(sql, "ck_evidences_content_kind");
        for k in all_kinds() {
            let t = tag(k, "content_kind").unwrap();
            assert!(
                kinds.contains(&format!("'{t}'")),
                "{t} missing from {kinds}"
            );
        }
        for constraint in ["ck_evidences_action", "ck_evidences_suggested_action"] {
            let actions = check_list(sql, constraint);
            for a in all_actions() {
                let t = tag(a, "action").unwrap();
                assert!(
                    actions.contains(&format!("'{t}'")),
                    "{t} missing from {actions}"
                );
            }
        }
        let severities = check_list(sql, "ck_evidences_severity");
        for s in all_severities() {
            let t = tag(s, "severity").unwrap();
            assert!(severities.contains(&format!("'{t}'")), "{t} missing");
        }
    }
}

fn row(kind: ContentKind, action: VerdictAction, severity: Severity) -> EvidenceRow {
    let now = Utc::now();
    EvidenceRow {
        trace_id: Uuid::now_v7(),
        ordinal: 7,
        tenant_id: Uuid::now_v7(),
        correlation_id: Some("corr".to_owned()),
        connector_id: "conn".to_owned(),
        content_kind: kind,
        created_at: now,
        expires_at: now + TimeDelta::days(1),
        action,
        settings_schema: Some("1.0".to_owned()),
        finding: Finding {
            detector: "gts.x".to_owned(),
            category: "pii.email".to_owned(),
            severity,
            confidence: 0.5,
            span: Some(Span { start: 1, end: 4 }),
            suggested_action: action,
            detector_version: Some("1".to_owned()),
        },
        excerpt: Some(Excerpt {
            text: "abc".to_owned(),
            start: 0,
        }),
    }
}

#[test]
fn every_variant_round_trips() {
    for kind in all_kinds() {
        for action in all_actions() {
            for severity in all_severities() {
                let original = row(kind, action, severity);
                let back = from_model(to_model(original.clone()).unwrap()).unwrap();
                assert_eq!(back, original);
            }
        }
    }
    let mut no_span = row(ContentKind::Prompt, VerdictAction::Block, Severity::High);
    no_span.finding.span = None;
    no_span.excerpt = None;
    no_span.correlation_id = None;
    assert_eq!(
        from_model(to_model(no_span.clone()).unwrap()).unwrap(),
        no_span
    );
}

#[test]
fn values_the_domain_cannot_hold_are_mapping_errors() {
    let mut model = to_model(row(
        ContentKind::Prompt,
        VerdictAction::Block,
        Severity::High,
    ))
    .unwrap();
    model.action = "explode".to_owned();
    assert!(matches!(
        from_model(model.clone()),
        Err(DomainError::Database { .. })
    ));
    model.action = "block".to_owned();
    model.ordinal = 300;
    assert!(from_model(model.clone()).is_err());
    model.ordinal = 1;
    model.span_start = Some(-1);
    assert!(from_model(model).is_err());

    let mut too_far = row(ContentKind::Prompt, VerdictAction::Block, Severity::High);
    too_far.finding.span = Some(Span {
        start: u32::MAX,
        end: u32::MAX,
    });
    assert!(to_model(too_far).is_err());
}
