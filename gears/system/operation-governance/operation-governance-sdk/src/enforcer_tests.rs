#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::facts::{FactError, FactProvider, FactWriter};
use crate::gts::GovernedOperationV1;
use crate::models::{
    CheckMode, CheckResponse, FactSet, GovernanceOutcome, GovernedResource, PolicyFinding,
};
use crate::test_support::{ScriptedPlugin, check_provider_covers};

use super::{EnforcerConfig, GovernanceEnforcer};

#[toolkit_gts::gts_type_schema(
    dir_path = "schemas",
    base = GovernedOperationV1,
    type_id = toolkit_gts::gts_id!("cf.core.og.governed_operation.v1~cf.test.thing.delete.v1~"),
    description = "test operation",
    properties = "name,age_seconds",
    traits = serde_json::json!({ "facts": { "age_seconds": { "tier": "costly" } } })
)]
#[derive(Default)]
struct DeleteThingV1 {
    name: String,
    age_seconds: i64,
}

/// Counts how often each tier was asked for.
#[derive(Default)]
struct Provider {
    calls: AtomicUsize,
    loads: AtomicUsize,
}

#[async_trait]
impl FactProvider for Provider {
    type Facts = DeleteThingV1;

    async fn provide(&self, need: &FactSet, out: &mut FactWriter<'_>) -> Result<(), FactError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        for fact in need {
            match fact.as_str() {
                "name" => out.set(fact, "report.pdf")?,
                "age_seconds" => {
                    self.loads.fetch_add(1, Ordering::SeqCst);
                    out.set(fact, 30)?;
                }
                other => return Err(FactError::Undeclared(other.to_owned())),
            }
        }
        Ok(())
    }
}

fn ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(Uuid::new_v4())
        .build()
        .unwrap()
}

fn resource() -> GovernedResource {
    GovernedResource::new("cf.test.thing", Uuid::new_v4()).with_id("t1")
}

fn enforcer(plugin: &Arc<ScriptedPlugin>) -> GovernanceEnforcer {
    GovernanceEnforcer::with_plugin(plugin.clone(), EnforcerConfig::default())
}

#[tokio::test]
async fn ungoverned_operation_loads_no_fact_and_makes_no_check() {
    let plugin = Arc::new(ScriptedPlugin::not_governed());
    let provider = Provider::default();
    let outcome = enforcer(&plugin)
        .check(&ctx(), resource(), CheckMode::Enforce, &provider)
        .await;
    assert_eq!(outcome, GovernanceOutcome::Allowed);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    assert!(plugin.requests().is_empty());
}

#[tokio::test]
async fn loads_only_facts_read_and_returns_the_decision() {
    let blocked = GovernanceOutcome::Blocked {
        findings: vec![PolicyFinding {
            policy: "p".into(),
            message: "no".into(),
        }],
    };
    let plugin = Arc::new(
        ScriptedPlugin::governed(["name"]).then(Ok(CheckResponse::Decided(blocked.clone()))),
    );
    let provider = Provider::default();
    let outcome = enforcer(&plugin)
        .check(&ctx(), resource(), CheckMode::Enforce, &provider)
        .await;
    assert_eq!(outcome, blocked);
    assert_eq!(
        provider.loads.load(Ordering::SeqCst),
        0,
        "costly tier not read"
    );
    let requests = plugin.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].facts.len(), 1);
}

#[tokio::test]
async fn missing_facts_are_loaded_once_then_checked_again() {
    let plugin = Arc::new(
        ScriptedPlugin::governed(["name"])
            .then(Ok(CheckResponse::NeedFacts {
                missing: ["age_seconds".to_owned()].into(),
            }))
            .then(Ok(CheckResponse::Decided(GovernanceOutcome::Allowed))),
    );
    let provider = Provider::default();
    let outcome = enforcer(&plugin)
        .check(&ctx(), resource(), CheckMode::Enforce, &provider)
        .await;
    assert_eq!(outcome, GovernanceOutcome::Allowed);
    let requests = plugin.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].round, 1);
    assert_eq!(requests[1].facts["age_seconds"], 30);
}

#[tokio::test]
async fn second_missing_facts_answer_is_unavailable() {
    let need = || {
        Ok(CheckResponse::NeedFacts {
            missing: ["age_seconds".to_owned()].into(),
        })
    };
    let plugin = Arc::new(ScriptedPlugin::governed(["name"]).then(need()).then(need()));
    let outcome = enforcer(&plugin)
        .check(&ctx(), resource(), CheckMode::Enforce, &Provider::default())
        .await;
    assert!(matches!(outcome, GovernanceOutcome::Unavailable { .. }));
}

#[tokio::test]
async fn backend_failure_is_never_allowed() {
    let plugin = Arc::new(
        ScriptedPlugin::governed(["name"]).then(Err(crate::OgError::Unavailable("down".into()))),
    );
    let outcome = enforcer(&plugin)
        .check(&ctx(), resource(), CheckMode::Enforce, &Provider::default())
        .await;
    assert!(matches!(
        outcome,
        GovernanceOutcome::Unavailable {
            retryable: true,
            ..
        }
    ));
}

#[tokio::test]
async fn applicability_is_cached_per_tenant() {
    let plugin = Arc::new(ScriptedPlugin::not_governed());
    let enforcer = enforcer(&plugin);
    let target = resource();
    for _ in 0..3 {
        enforcer
            .check(
                &ctx(),
                target.clone(),
                CheckMode::Enforce,
                &Provider::default(),
            )
            .await;
    }
    assert_eq!(plugin.applicability_calls(), 1);
}

#[tokio::test]
async fn provider_covers_every_declared_fact() {
    check_provider_covers(&Provider::default()).await.unwrap();
}

#[tokio::test]
async fn zero_lease_is_never_cached() {
    let plugin = Arc::new(ScriptedPlugin::not_governed().with_lease_ms(0));
    let enforcer = enforcer(&plugin);
    let target = resource();
    for _ in 0..3 {
        enforcer
            .check(
                &ctx(),
                target.clone(),
                CheckMode::Enforce,
                &Provider::default(),
            )
            .await;
    }
    assert_eq!(plugin.applicability_calls(), 3);
}

#[tokio::test]
async fn lease_is_capped_by_the_configured_maximum() {
    let plugin = Arc::new(ScriptedPlugin::not_governed().with_lease_ms(3_600_000));
    let enforcer = GovernanceEnforcer::with_plugin(
        plugin.clone(),
        EnforcerConfig {
            max_applicability_lease: std::time::Duration::from_millis(20),
            ..EnforcerConfig::default()
        },
    );
    let target = resource();
    enforcer
        .check(
            &ctx(),
            target.clone(),
            CheckMode::Enforce,
            &Provider::default(),
        )
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    enforcer
        .check(&ctx(), target, CheckMode::Enforce, &Provider::default())
        .await;
    assert_eq!(plugin.applicability_calls(), 2, "capped lease expired");
}
