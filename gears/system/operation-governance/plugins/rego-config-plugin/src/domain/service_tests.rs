#![allow(clippy::unwrap_used, clippy::expect_used)]

use operation_governance_sdk::{
    CheckMode, CheckResponse, GovernanceCheckRequest, GovernanceOutcome, GovernedResource,
};
use serde_json::json;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::{OgRegoConfigPluginConfig, OperationPolicies, PolicyConfig, PolicyKind};

use super::Service;

const DELETE_V1: &str = "gts.cf.core.og.governed_operation.v1~cf.test.object.delete.v1~";
const DELETE_V2: &str = "gts.cf.core.og.governed_operation.v1~cf.test.object.delete.v2~";

const RETENTION: &str = r#"
package object.retention

deny contains "objects younger than one hour cannot be deleted" if {
    input.facts.age_seconds < 3600
}
"#;

const LARGE: &str = r#"
package object.large

require_approval contains "deleting a large object needs approval" if {
    input.facts.size_bytes > 1000
}
"#;

fn policy(name: &str, kind: PolicyKind, source: &str) -> PolicyConfig {
    PolicyConfig {
        name: name.to_owned(),
        kind,
        source: source.to_owned(),
    }
}

fn service() -> Service {
    Service::from_config(&OgRegoConfigPluginConfig {
        operations: vec![OperationPolicies {
            operation: DELETE_V1.to_owned(),
            policies: vec![
                policy("retention", PolicyKind::Guardrail, RETENTION),
                policy("large", PolicyKind::Approval, LARGE),
            ],
        }],
        ..OgRegoConfigPluginConfig::default()
    })
    .unwrap()
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap()
}

fn request(operation: &str, facts: &serde_json::Value, round: u8) -> GovernanceCheckRequest {
    GovernanceCheckRequest {
        operation: operation.to_owned(),
        resource: GovernedResource::new("cf.test.object", Uuid::new_v4()),
        facts: facts.as_object().unwrap().clone(),
        unavailable: std::collections::BTreeMap::default(),
        mode: CheckMode::Enforce,
        round,
    }
}

fn decided(response: CheckResponse) -> GovernanceOutcome {
    match response {
        CheckResponse::Decided(outcome) => outcome,
        other @ CheckResponse::NeedFacts { .. } => panic!("expected a decision, got {other:?}"),
    }
}

#[test]
fn applicability_lists_facts_read_by_all_policies() {
    let a = service().applicability(DELETE_V1);
    assert!(a.governed);
    assert_eq!(
        a.facts_read.into_iter().collect::<Vec<_>>(),
        ["age_seconds", "size_bytes"]
    );
}

#[test]
fn applicability_carries_the_configured_lease() {
    assert_eq!(
        service().applicability(DELETE_V1).lease_ms,
        crate::config::DEFAULT_APPLICABILITY_LEASE_MS
    );
    let other = "gts.cf.core.og.governed_operation.v1~cf.test.object.create.v1~";
    assert_eq!(
        service().applicability(other).lease_ms,
        crate::config::DEFAULT_APPLICABILITY_LEASE_MS,
        "a not-governed answer is leased too"
    );
}

#[test]
fn lease_above_the_maximum_is_refused_at_startup() {
    let err = Service::from_config(&OgRegoConfigPluginConfig {
        applicability_lease_ms: crate::config::MAX_APPLICABILITY_LEASE_MS + 1,
        ..OgRegoConfigPluginConfig::default()
    });
    assert!(err.is_err());
}

#[test]
fn new_type_version_keeps_matching_the_family() {
    assert!(service().applicability(DELETE_V2).governed);
}

#[test]
fn unknown_operation_is_not_governed_and_allowed() {
    let s = service();
    let other = "gts.cf.core.og.governed_operation.v1~cf.test.object.create.v1~";
    assert!(!s.applicability(other).governed);
    assert_eq!(
        decided(s.check(&ctx(), &request(other, &json!({}), 0)).unwrap()),
        GovernanceOutcome::Allowed
    );
}

#[test]
fn guardrail_blocks_before_approval_is_asked() {
    let outcome = decided(
        service()
            .check(
                &ctx(),
                &request(
                    DELETE_V1,
                    &json!({"age_seconds": 10, "size_bytes": 5000}),
                    0,
                ),
            )
            .unwrap(),
    );
    let GovernanceOutcome::Blocked { findings } = outcome else {
        panic!("expected Blocked, got {outcome:?}");
    };
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].policy, "retention");
}

#[test]
fn approval_policy_requires_approval() {
    let outcome = decided(
        service()
            .check(
                &ctx(),
                &request(
                    DELETE_V1,
                    &json!({"age_seconds": 7200, "size_bytes": 5000}),
                    0,
                ),
            )
            .unwrap(),
    );
    assert!(matches!(
        outcome,
        GovernanceOutcome::ApprovalRequired { .. }
    ));
}

#[test]
fn passing_facts_are_allowed() {
    let outcome = decided(
        service()
            .check(
                &ctx(),
                &request(
                    DELETE_V1,
                    &json!({"age_seconds": 7200, "size_bytes": 10}),
                    0,
                ),
            )
            .unwrap(),
    );
    assert_eq!(outcome, GovernanceOutcome::Allowed);
}

#[test]
fn missing_facts_are_asked_once_then_unavailable() {
    let s = service();
    let first = s
        .check(
            &ctx(),
            &request(DELETE_V1, &json!({"age_seconds": 7200}), 0),
        )
        .unwrap();
    assert_eq!(
        first,
        CheckResponse::NeedFacts {
            missing: ["size_bytes".to_owned()].into()
        }
    );
    let second = decided(
        s.check(
            &ctx(),
            &request(DELETE_V1, &json!({"age_seconds": 7200}), 1),
        )
        .unwrap(),
    );
    assert!(matches!(second, GovernanceOutcome::Unavailable { .. }));
}

#[test]
fn unavailable_fact_read_by_a_policy_is_unavailable() {
    let mut req = request(DELETE_V1, &json!({"size_bytes": 10}), 0);
    req.unavailable
        .insert("age_seconds".to_owned(), "db down".to_owned());
    let outcome = decided(service().check(&ctx(), &req).unwrap());
    assert!(matches!(outcome, GovernanceOutcome::Unavailable { .. }));
}

#[test]
fn dynamic_fact_reference_is_refused_at_startup() {
    let source = "package p\n\ndeny contains k if { some k; input.facts[k] == 1 }\n";
    let err = Service::from_config(&OgRegoConfigPluginConfig {
        operations: vec![OperationPolicies {
            operation: DELETE_V1.to_owned(),
            policies: vec![policy("dyn", PolicyKind::Guardrail, source)],
        }],
        ..OgRegoConfigPluginConfig::default()
    });
    assert!(err.is_err());
}

#[test]
fn wrong_entrypoint_is_refused_at_startup() {
    let err = Service::from_config(&OgRegoConfigPluginConfig {
        operations: vec![OperationPolicies {
            operation: DELETE_V1.to_owned(),
            policies: vec![policy("p", PolicyKind::Approval, RETENTION)],
        }],
        ..OgRegoConfigPluginConfig::default()
    });
    assert!(err.is_err());
}
