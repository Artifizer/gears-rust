//! Service tests: real SQLite, a tenant-clamping PDP, and real Rego policies
//! through the Rego config plugin.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use authz_resolver_sdk::constraints::{Constraint, InPredicate, Predicate};
use authz_resolver_sdk::models::{
    EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
};
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use object_storage_sdk::{NewObject, ObjectPatch};
use og_rego_config_plugin::config::{
    OgRegoConfigPluginConfig, OperationPolicies, PolicyConfig, PolicyKind,
};
use operation_governance_sdk::test_support::{ScriptedPlugin, check_provider_covers};
use operation_governance_sdk::{
    EnforcerConfig, GovernanceEnforcer, OgError, OperationGovernancePluginClientV1,
};
use time::OffsetDateTime;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_security::{PlatformSecurityContext, SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::governance::{ChangeView, GetOpFacts, ObjectView, UpdateOpFacts};
use crate::domain::service::{Service, ServiceConfig};
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::sea_orm_repo::SeaOrmObjectsRepository;
use object_storage_sdk::gts::{GetObjectV1Provider, UpdateObjectV1Provider};

type TestService = Service<SeaOrmObjectsRepository>;

const CREATE: &str = "gts.cf.core.og.governed_operation.v1~x.object_storage.object.create~";
const DELETE: &str = "gts.cf.core.og.governed_operation.v1~x.object_storage.object.delete~";

/// A PDP that allows everything inside the caller's tenant.
struct TenantPdp;

#[async_trait]
impl AuthZResolverApi for TenantPdp {
    async fn evaluate(
        &self,
        _ctx: PlatformSecurityContext,
        request: EvaluationRequest,
    ) -> Result<EvaluationResponse, CanonicalError> {
        let tenant = request
            .context
            .tenant_context
            .as_ref()
            .and_then(|tc| tc.root_id)
            .or_else(|| {
                request
                    .subject
                    .properties
                    .get("tenant_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            .ok_or_else(|| CanonicalError::internal("no tenant").create())?;
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint {
                    predicates: vec![Predicate::In(InPredicate::new(
                        pep_properties::OWNER_TENANT_ID,
                        [tenant],
                    ))],
                }],
                ..Default::default()
            },
        })
    }
}

async fn service_with(plugin: Arc<dyn OperationGovernancePluginClientV1>) -> TestService {
    use sea_orm_migration::MigratorTrait;
    let db = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .unwrap();
    Service::new(
        Arc::new(DBProvider::new(db)),
        Arc::new(SeaOrmObjectsRepository::new()),
        PolicyEnforcer::new(Arc::new(TenantPdp)),
        Arc::new(GovernanceEnforcer::with_plugin(
            plugin,
            EnforcerConfig::default(),
        )),
        ServiceConfig::default(),
    )
}

/// The service, governed by the given Rego policies.
async fn service_with_policies(operations: Vec<(&str, PolicyKind, &str, &str)>) -> TestService {
    let mut by_op: BTreeMap<&str, Vec<PolicyConfig>> = BTreeMap::new();
    for (op, kind, name, source) in operations {
        by_op.entry(op).or_default().push(PolicyConfig {
            name: name.to_owned(),
            kind,
            source: source.to_owned(),
        });
    }
    let plugin = og_rego_config_plugin::domain::Service::from_config(&OgRegoConfigPluginConfig {
        operations: by_op
            .into_iter()
            .map(|(operation, policies)| OperationPolicies {
                operation: operation.to_owned(),
                policies,
            })
            .collect(),
        ..OgRegoConfigPluginConfig::default()
    })
    .unwrap();
    service_with(Arc::new(plugin)).await
}

fn ctx_in(tenant: Uuid) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(tenant)
        .build()
        .unwrap()
}

fn new_object(content_type: &str, body: &str) -> NewObject {
    NewObject {
        name: "report".to_owned(),
        content_type: content_type.to_owned(),
        labels: BTreeMap::new(),
        body: body.to_owned(),
    }
}

#[tokio::test]
async fn ungoverned_crud_works_and_makes_no_check() {
    let plugin = Arc::new(ScriptedPlugin::not_governed());
    let svc = service_with(plugin.clone()).await;
    let ctx = ctx_in(Uuid::new_v4());

    let created = svc
        .create_object(&ctx, new_object("text/plain", "hello"))
        .await
        .unwrap();
    assert_eq!(created.size_bytes, 5);
    assert_eq!(created.created_at, created.updated_at);

    let read = svc.get_object(&ctx, created.id).await.unwrap();
    assert_eq!(read, created);

    let changed = svc
        .update_object(
            &ctx,
            created.id,
            ObjectPatch {
                body: Some("hello world".to_owned()),
                ..ObjectPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(changed.size_bytes, 11);

    svc.delete_object(&ctx, created.id).await.unwrap();
    assert!(matches!(
        svc.get_object(&ctx, created.id).await,
        Err(DomainError::NotFound(_))
    ));
    assert!(plugin.requests().is_empty(), "fast path: no check made");
}

#[tokio::test]
async fn guardrail_on_request_fact_blocks_create() {
    let svc = service_with_policies(vec![(
        CREATE,
        PolicyKind::Guardrail,
        "no-executables",
        r#"
package object.no_executables
deny contains "executables cannot be stored" if {
    input.facts.content_type == "application/x-msdownload"
}
"#,
    )])
    .await;
    let ctx = ctx_in(Uuid::new_v4());

    let err = svc
        .create_object(&ctx, new_object("application/x-msdownload", "MZ"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, DomainError::GovernanceBlocked(m) if m.contains("executables")),
        "{err:?}"
    );
    svc.create_object(&ctx, new_object("text/plain", "ok"))
        .await
        .unwrap();
}

#[tokio::test]
async fn approval_policy_on_computed_fact_requires_approval() {
    let svc = service_with_policies(vec![(
        CREATE,
        PolicyKind::Approval,
        "large-objects",
        r#"
package object.large
require_approval contains "objects over 10 bytes need approval" if {
    input.facts.size_bytes > 10
}
"#,
    )])
    .await;
    let ctx = ctx_in(Uuid::new_v4());
    let err = svc
        .create_object(&ctx, new_object("text/plain", "far more than ten bytes"))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::ApprovalRequired(_)), "{err:?}");
}

#[tokio::test]
async fn costly_fact_is_counted_on_demand() {
    // An illustration of a costly Fact. Real limits belong to Quota
    // Enforcement: Facts are read without locks.
    let svc = service_with_policies(vec![(
        CREATE,
        PolicyKind::Guardrail,
        "two-per-owner",
        r#"
package object.per_owner
deny contains "an owner may keep at most two objects" if {
    input.facts.owner_object_count >= 2
}
"#,
    )])
    .await;
    let ctx = ctx_in(Uuid::new_v4());
    for _ in 0..2 {
        svc.create_object(&ctx, new_object("text/plain", "x"))
            .await
            .unwrap();
    }
    let err = svc
        .create_object(&ctx, new_object("text/plain", "x"))
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::GovernanceBlocked(_)), "{err:?}");
}

#[tokio::test]
async fn retention_policy_on_generated_created_at_blocks_delete() {
    let svc = service_with_policies(vec![(
        DELETE,
        PolicyKind::Guardrail,
        "retention",
        r#"
package object.retention
deny contains "objects younger than one hour cannot be deleted" if {
    input.facts.age_seconds < 3600
}
"#,
    )])
    .await;
    let ctx = ctx_in(Uuid::new_v4());
    let created = svc
        .create_object(&ctx, new_object("text/plain", "keep me"))
        .await
        .unwrap();

    let err = svc.delete_object(&ctx, created.id).await.unwrap_err();
    assert!(
        matches!(&err, DomainError::GovernanceBlocked(m) if m.contains("one hour")),
        "{err:?}"
    );
    // Still there.
    svc.get_object(&ctx, created.id).await.unwrap();

    // A missing object is "not found", not a governance refusal.
    assert!(matches!(
        svc.delete_object(&ctx, Uuid::new_v4()).await,
        Err(DomainError::NotFound(_))
    ));
}

#[tokio::test]
async fn governance_failure_never_lets_the_operation_run() {
    let plugin = Arc::new(
        ScriptedPlugin::governed(["content_type"])
            .then(Err(OgError::Unavailable("backend down".to_owned()))),
    );
    let svc = service_with(plugin).await;
    let ctx = ctx_in(Uuid::new_v4());
    let err = svc
        .create_object(&ctx, new_object("text/plain", "x"))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            DomainError::GovernanceUnavailable {
                retryable: true,
                ..
            }
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn fact_providers_cover_every_declared_fact() {
    let now = OffsetDateTime::now_utc();
    let object = object_storage_sdk::StoredObject {
        id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        owner_id: Uuid::new_v4(),
        name: "n".to_owned(),
        content_type: "text/plain".to_owned(),
        size_bytes: 1,
        labels: BTreeMap::new(),
        body: "b".to_owned(),
        created_at: now,
        updated_at: now,
    };
    check_provider_covers(&GetObjectV1Provider(GetOpFacts {
        view: ObjectView {
            object: &object,
            caller: object.owner_id,
            now,
        },
    }))
    .await
    .unwrap();
    check_provider_covers(&UpdateObjectV1Provider(UpdateOpFacts {
        change: ChangeView {
            before: &object,
            after: &object,
            caller: object.owner_id,
            now,
        },
    }))
    .await
    .unwrap();
}
