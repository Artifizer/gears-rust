#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: a failed expectation is the test failing"
)]

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use authz_resolver_sdk::PolicyEnforcer;
use file_parser_sdk::FileParserClientError;
use prompt_scanner_sdk::{
    Attachment, ContentKind, PolicyProviderSpecV1, ScanJobFailureCode, ScanJobStatus, ScanPhase,
    ScanRequest, TenantPolicy, UnscoredBackend, UnscoredReason, VerdictAction,
};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::gts::PluginV1;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use crate::config::FailMode;
use crate::domain::error::DomainError;
use crate::domain::metrics::EvidenceMetricsPort;
use crate::domain::policy_provider::ConfigPolicyProvider;
use crate::domain::repos::{EvidenceRepository, EvidenceRow};
use crate::domain::retention::EvidenceMaintenanceJobs;
use crate::domain::test_support::*;
use crate::infra::storage::OrmEvidenceRepository;

use super::Service;

/// A check on an error, for table-driven tests.
type ErrorCheck = fn(&DomainError) -> bool;
/// A change to a policy, for table-driven tests.
type PolicyEdit = Box<dyn Fn(&mut TenantPolicy)>;

fn scan_request() -> ScanRequest {
    ScanRequest::text("test-connector", ContentKind::Prompt, "hello world")
}

fn flagging() -> Arc<FakeBackend> {
    Arc::new(FakeBackend {
        category: "pii.email".to_owned(),
        action: VerdictAction::Block,
    })
}

const FLAGGING_VENDOR: &str = "flagging_vendor";

/// A hub with one backend that flags every scan (`pii.email`, `block`).
fn hub_with_flagging_backend() -> Arc<ClientHub> {
    hub_with(vec![(FLAGGING_VENDOR, flagging())])
}

async fn put_policy_with<R: EvidenceRepository + 'static>(
    svc: &Service<R>,
    ctx: &SecurityContext,
    enabled: &[&str],
    adjust: impl FnOnce(&mut TenantPolicy),
) {
    let mut policy = TenantPolicy::default_for_tenant(ctx.subject_tenant_id());
    policy.enabled_backends = enabled.iter().map(|s| (*s).to_owned()).collect();
    adjust(&mut policy);
    svc.put_policy(ctx, policy).await.expect("put_policy");
}

async fn enable_flagging_backend<R: EvidenceRepository + 'static>(
    svc: &Service<R>,
    ctx: &SecurityContext,
) {
    put_policy_with(svc, ctx, &[FLAGGING_VENDOR], |_| {}).await;
}

/// Every row of a scan, expired or not, read straight from the table.
async fn stored_rows(svc: &TestService, tenant_id: Uuid, trace_id: Uuid) -> Vec<EvidenceRow> {
    let conn = svc.db.conn().unwrap();
    svc.repo
        .find_by_trace(
            &conn,
            &AccessScope::for_tenant(tenant_id),
            trace_id,
            chrono::DateTime::<chrono::Utc>::MIN_UTC,
        )
        .await
        .unwrap()
}

// ── Fan-out and aggregation ──────────────────────────────────────────────

#[tokio::test]
async fn fan_out_aggregates_block_over_warn() {
    let hub = Arc::new(ClientHub::new());
    let mut instances = Vec::new();
    let warn_gts_id = register_backend(
        &hub,
        &mut instances,
        "warn_vendor",
        Arc::new(FakeBackend {
            category: "pii.email".to_owned(),
            action: VerdictAction::Warn,
        }),
    );
    register_backend(&hub, &mut instances, "block_vendor", flagging());
    register_registry(&hub, instances);
    let svc = make_service(hub).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(&svc, &ctx, &["warn_vendor", "block_vendor"], |_| {}).await;

    let verdict = svc.scan(&ctx, scan_request()).await.expect("scan");
    assert_eq!(verdict.action, VerdictAction::Block);
    assert_eq!(verdict.findings.len(), 2);
    let warn = verdict
        .findings
        .iter()
        .find(|f| f.action_is(VerdictAction::Warn))
        .unwrap();
    assert_eq!(
        warn.detector, warn_gts_id,
        "detector is the GTS instance id"
    );
    assert_eq!(warn.detector_version.as_deref(), Some(FAKE_VERSION));
}

trait ActionIs {
    fn action_is(&self, a: VerdictAction) -> bool;
}

impl ActionIs for prompt_scanner_sdk::Finding {
    fn action_is(&self, a: VerdictAction) -> bool {
        self.suggested_action == a
    }
}

/// A scan of a content kind the policy does not govern runs no backends, is
/// allowed and stores nothing; a governed kind is scanned as usual.
#[tokio::test]
async fn ungoverned_content_kind_is_allowed_without_scanning() {
    let svc = make_service(hub_with_flagging_backend()).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(&svc, &ctx, &[FLAGGING_VENDOR], |p| {
        p.content_kinds = vec![ContentKind::Response];
    })
    .await;
    assert_eq!(
        svc.get_policy(&ctx).await.unwrap().content_kinds,
        vec![ContentKind::Response]
    );

    let prompt = svc.scan(&ctx, scan_request()).await.unwrap();
    assert_eq!(prompt.action, VerdictAction::Allow);
    assert!(prompt.findings.is_empty());
    svc.wait_for_pending_writes().await;
    assert!(matches!(
        svc.get_verdict(&ctx, prompt.trace_id).await,
        Err(DomainError::VerdictNotFound { .. })
    ));

    let mut response = scan_request();
    response.content_kind = ContentKind::Response;
    let response = svc.scan(&ctx, response).await.unwrap();
    assert_eq!(response.action, VerdictAction::Block);
}

#[tokio::test]
async fn a_redact_verdict_returns_the_redacted_payload_once() {
    let svc = make_service(hub_with(vec![(
        "redactor",
        Arc::new(FakeBackend {
            category: "pii.name".to_owned(),
            action: VerdictAction::Redact,
        }),
    )]))
    .await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(&svc, &ctx, &["redactor"], |_| {}).await;

    let verdict = svc.scan(&ctx, scan_request()).await.unwrap();
    assert_eq!(verdict.action, VerdictAction::Redact);
    let redacted = verdict.redacted_payload.expect("redact verdict carries it");
    assert!(!redacted.contains("hello"), "{redacted}");
    assert!(redacted.ends_with(" world"), "{redacted}");

    svc.wait_for_pending_writes().await;
    let stored = svc.get_verdict(&ctx, verdict.trace_id).await.unwrap();
    assert_eq!(stored.redacted_payload, None, "never stored");
}

#[tokio::test]
async fn a_slow_or_failing_backend_degrades_to_unscored_when_not_required() {
    let svc = make_service(hub_with(vec![
        ("slow_vendor", Arc::new(SlowBackend)),
        ("broken_vendor", Arc::new(FailingBackend)),
        (
            "fast_vendor",
            Arc::new(FakeBackend {
                category: "pii.email".to_owned(),
                action: VerdictAction::Audit,
            }),
        ),
    ]))
    .await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(
        &svc,
        &ctx,
        &["slow_vendor", "broken_vendor", "fast_vendor"],
        |_| {},
    )
    .await;

    let verdict = svc.scan(&ctx, scan_request()).await.unwrap();
    assert_eq!(verdict.findings.len(), 1);
    assert_eq!(verdict.action, VerdictAction::Audit);
}

/// A non-required backend that times out, errors, reports itself
/// unavailable or has no instance is listed on the verdict and counted; the
/// verdict action is unchanged (default `Allow` here, fail mode `closed`).
#[tokio::test]
async fn unscored_backends_are_listed_counted_and_do_not_change_the_action() {
    let metrics = Arc::new(CountingMetrics::default());
    let mut wiring = Wiring::new(hub_with(vec![
        ("slow_vendor", Arc::new(SlowBackend)),
        ("broken_vendor", Arc::new(FailingBackend)),
        ("modelless_vendor", Arc::new(UnavailableBackend)),
    ]))
    .await;
    wiring.metrics = Arc::clone(&metrics) as Arc<dyn EvidenceMetricsPort>;
    let svc = wiring.build();
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(
        &svc,
        &ctx,
        &[
            "slow_vendor",
            "broken_vendor",
            "modelless_vendor",
            "ghost_vendor",
        ],
        |_| {},
    )
    .await;

    let verdict = svc.scan(&ctx, scan_request()).await.unwrap();
    assert_eq!(verdict.action, VerdictAction::Allow);
    assert!(verdict.findings.is_empty());
    let mut unscored: Vec<(String, UnscoredReason)> = verdict
        .unscored_backends
        .iter()
        .map(|u| (u.backend.clone(), u.reason))
        .collect();
    unscored.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        unscored,
        [
            ("broken_vendor".to_owned(), UnscoredReason::Error),
            ("ghost_vendor".to_owned(), UnscoredReason::Unavailable),
            ("modelless_vendor".to_owned(), UnscoredReason::Unavailable),
            ("slow_vendor".to_owned(), UnscoredReason::Timeout),
        ]
    );
    let mut counted = metrics.unscored.lock().unwrap().clone();
    counted.sort_by_key(|r| r.as_str());
    assert_eq!(
        counted,
        [
            UnscoredReason::Error,
            UnscoredReason::Timeout,
            UnscoredReason::Unavailable,
            UnscoredReason::Unavailable,
        ]
    );
}

/// Full coverage lists nothing.
#[tokio::test]
async fn a_fully_scored_verdict_lists_no_unscored_backends() {
    let svc = make_service(hub_with_flagging_backend()).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(&svc, &ctx, &[FLAGGING_VENDOR], |_| {}).await;
    let verdict = svc.scan(&ctx, scan_request()).await.unwrap();
    assert!(verdict.unscored_backends.is_empty());
}

#[tokio::test]
async fn a_required_backend_that_times_out_errors_or_is_missing_fails_the_scan() {
    let svc = make_service(hub_with(vec![
        ("slow_vendor", Arc::new(SlowBackend)),
        ("broken_vendor", Arc::new(FailingBackend)),
    ]))
    .await;
    let ctx = ctx_for_tenant(Uuid::new_v4());

    let cases: [(&str, ErrorCheck); 3] = [
        ("slow_vendor", |e| {
            matches!(e, DomainError::BackendTimeout { .. })
        }),
        ("broken_vendor", |e| {
            matches!(e, DomainError::RequiredBackendFailed { .. })
        }),
        ("ghost_vendor", |e| {
            matches!(e, DomainError::BackendUnavailable { .. })
        }),
    ];
    for (vendor, expected) in cases {
        put_policy_with(&svc, &ctx, &[vendor], |p| {
            p.required_backends = vec![vendor.to_owned()];
        })
        .await;
        let err = svc.scan(&ctx, scan_request()).await.expect_err(vendor);
        assert!(expected(&err), "{vendor}: {err:?}");
    }
}

/// The request names no tenant: a scan runs for, and is recorded under, the
/// caller's own tenant.
#[tokio::test]
async fn scan_runs_for_the_callers_tenant() {
    let svc = make_service(hub_with(Vec::new())).await;
    let tenant_id = Uuid::new_v4();
    let verdict = svc
        .scan(&ctx_for_tenant(tenant_id), scan_request())
        .await
        .unwrap();
    assert_eq!(verdict.tenant_id, tenant_id);
}

#[tokio::test]
async fn request_ids_over_255_characters_are_rejected() {
    let svc = make_service(hub_with_flagging_backend()).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());

    let mut req = scan_request();
    req.connector_id = "c".repeat(256);
    assert!(matches!(
        svc.scan(&ctx, req).await,
        Err(DomainError::Validation { .. })
    ));
    let mut req = scan_request();
    req.correlation_id = Some("r".repeat(256));
    assert!(matches!(
        svc.scan(&ctx, req).await,
        Err(DomainError::Validation { .. })
    ));
}

// ── Authorization ────────────────────────────────────────────────────────

/// A tenantless token (nil `subject_tenant_id`) has no tenant; every entry
/// point refuses it.
#[tokio::test]
async fn a_nil_tenant_is_refused_everywhere() {
    let svc = make_service(hub_with(Vec::new())).await;
    let ctx = ctx_for_tenant(Uuid::nil());
    let id = Uuid::now_v7();

    assert!(matches!(
        svc.scan(&ctx, scan_request()).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.scan_async(&ctx, scan_request()).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.get_verdict(&ctx, id).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.get_job_status(&ctx, id).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.delete_evidence(&ctx, id).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.find_by_correlation_id(&ctx, "x").await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.get_policy(&ctx).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.put_policy(&ctx, TenantPolicy::default_for_tenant(Uuid::nil()))
            .await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.list_backend_instances(&ctx).await,
        Err(DomainError::Forbidden)
    ));
}

/// A PDP deny is enforced on every entry point, before any work: a denied
/// `scan_async` queues nothing.
#[tokio::test]
async fn a_pdp_deny_is_enforced_on_every_entry_point() {
    let mut wiring = Wiring::new(hub_with_flagging_backend()).await;
    wiring.enforcer = PolicyEnforcer::new(Arc::new(DenyActions(&[
        "create", "get", "delete", "update", "list",
    ])));
    let svc = wiring.build();
    let ctx = ctx_for_tenant(Uuid::new_v4());
    let id = Uuid::now_v7();

    assert!(matches!(
        svc.scan(&ctx, scan_request()).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.scan_async(&ctx, scan_request()).await,
        Err(DomainError::Forbidden)
    ));
    assert!(!svc.process_next_job().await, "nothing was queued");
    assert!(matches!(
        svc.get_verdict(&ctx, id).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.get_job_status(&ctx, id).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.delete_evidence(&ctx, id).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.get_policy(&ctx).await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.put_policy(&ctx, TenantPolicy::default_for_tenant(Uuid::nil()))
            .await,
        Err(DomainError::Forbidden)
    ));
    assert!(matches!(
        svc.list_backend_instances(&ctx).await,
        Err(DomainError::Forbidden)
    ));
}

/// A PDP that allows a row access without constraints is refused, not
/// narrowed: rows are only ever read under a PDP-given tenant constraint.
#[tokio::test]
async fn an_unconstrained_pdp_decision_is_refused_for_row_access() {
    let db = in_memory_db().await;
    let hub = hub_with_flagging_backend();
    let writer = Wiring::over(Arc::clone(&db), Arc::clone(&hub)).build();
    let mut reader = Wiring::over(db, hub);
    reader.enforcer = PolicyEnforcer::new(Arc::new(UnconstrainedAllowResolver));
    let reader = reader.build();

    let ctx_a = ctx_for_tenant(Uuid::new_v4());
    enable_flagging_backend(&writer, &ctx_a).await;
    let verdict = writer.scan(&ctx_a, scan_request()).await.unwrap();
    writer.wait_for_pending_writes().await;

    for ctx in [ctx_a, ctx_for_tenant(Uuid::new_v4())] {
        assert!(matches!(
            reader.get_verdict(&ctx, verdict.trace_id).await,
            Err(DomainError::Forbidden)
        ));
        assert!(matches!(
            reader.scan(&ctx, scan_request()).await,
            Err(DomainError::Forbidden)
        ));
    }
}

#[tokio::test]
async fn tenant_isolation_get_verdict() {
    let svc = make_service(hub_with_flagging_backend()).await;
    let ctx_a = ctx_for_tenant(Uuid::new_v4());
    let ctx_b = ctx_for_tenant(Uuid::new_v4());
    enable_flagging_backend(&svc, &ctx_a).await;

    let verdict = svc.scan(&ctx_a, scan_request()).await.unwrap();
    svc.wait_for_pending_writes().await;
    assert_eq!(
        svc.get_verdict(&ctx_a, verdict.trace_id)
            .await
            .unwrap()
            .trace_id,
        verdict.trace_id
    );
    assert!(matches!(
        svc.get_verdict(&ctx_b, verdict.trace_id).await,
        Err(DomainError::VerdictNotFound { .. })
    ));
}

// ── Policies ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn put_policy_refuses_another_tenants_id_and_fills_a_nil_one() {
    let svc = make_service(hub_with(Vec::new())).await;
    let tenant = Uuid::new_v4();
    let ctx = ctx_for_tenant(tenant);

    let err = svc
        .put_policy(&ctx, TenantPolicy::default_for_tenant(Uuid::new_v4()))
        .await
        .expect_err("another tenant's id");
    assert!(matches!(err, DomainError::Validation { ref field, .. } if field == "tenant_id"));

    let stored = svc
        .put_policy(&ctx, TenantPolicy::default_for_tenant(Uuid::nil()))
        .await
        .unwrap();
    assert_eq!(stored.tenant_id, tenant);
}

#[test]
fn validate_policy_refuses_what_the_gateway_cannot_honour() {
    let base = || {
        let mut p = TenantPolicy::default_for_tenant(Uuid::nil());
        p.enabled_backends = vec!["a".to_owned()];
        p
    };
    TestService::validate_policy(&base()).expect("valid");

    let cases: Vec<(&str, PolicyEdit)> = vec![
        (
            "thresholds",
            Box::new(|p| {
                p.thresholds.insert("x".to_owned(), 1.5);
            }),
        ),
        (
            "thresholds",
            Box::new(|p| {
                p.thresholds.insert("x".to_owned(), f32::NAN);
            }),
        ),
        (
            "thresholds",
            Box::new(|p| {
                p.thresholds.insert(String::new(), 0.5);
            }),
        ),
        (
            "required_backends",
            Box::new(|p| {
                p.required_backends = vec!["b".to_owned()];
            }),
        ),
        (
            "enabled_backends",
            Box::new(|p| {
                p.enabled_backends = (0..65).map(|i| format!("v{i}")).collect();
            }),
        ),
        (
            "enabled_backends",
            Box::new(|p| {
                p.enabled_backends = vec!["x".repeat(129)];
            }),
        ),
        (
            "category_action_overrides",
            Box::new(|p| {
                p.category_action_overrides
                    .insert(String::new(), VerdictAction::Allow);
            }),
        ),
        (
            "retention",
            Box::new(|p| p.retention = Some(Duration::ZERO)),
        ),
        (
            "settings_schema",
            Box::new(|p| {
                p.settings_schema = Some("x".repeat(17));
            }),
        ),
        (
            "content_kinds",
            Box::new(|p| {
                p.content_kinds = vec![ContentKind::Prompt; 6];
            }),
        ),
    ];
    for (field, adjust) in cases {
        let mut p = base();
        adjust(&mut p);
        match TestService::validate_policy(&p) {
            Err(DomainError::Validation { field: f, .. }) => assert_eq!(f, field),
            other => panic!("{field}: expected a validation error, got {other:?}"),
        }
    }
}

/// Registers a `PolicyProviderSpecV1` instance in `instances`, with
/// `client` in the hub when given. Returns the instance JSON.
fn register_provider(
    hub: &ClientHub,
    instances: &mut Vec<serde_json::Value>,
    client: Option<Arc<dyn prompt_scanner_sdk::PolicyProvider>>,
) -> serde_json::Value {
    let (id, json) = PluginV1::<PolicyProviderSpecV1>::build_registration(
        "cf.test.policy.provider.v1",
        "test_policy_vendor",
        10,
    )
    .unwrap();
    if let Some(client) = client {
        hub.register_scoped::<dyn prompt_scanner_sdk::PolicyProvider>(
            ClientScope::gts_id(&id),
            client,
        );
    }
    instances.push(json.clone());
    json
}

/// A registered provider plugin wins over the built-in fallback.
#[tokio::test]
async fn a_registered_policy_provider_plugin_wins_over_the_fallback() {
    let hub = Arc::new(ClientHub::new());
    let mut instances = Vec::new();
    register_backend(&hub, &mut instances, FLAGGING_VENDOR, flagging());
    let plugin = Arc::new(InMemoryPolicyProvider::default());
    register_provider(&hub, &mut instances, Some(plugin.clone()));
    register_registry(&hub, instances);

    let mut wiring = Wiring::new(hub).await;
    // The fallback would enable nothing.
    wiring.provider = Some(Arc::new(ConfigPolicyProvider::new(Some(
        TenantPolicy::default_for_tenant(Uuid::nil()),
    ))));
    let svc = wiring.build();
    let ctx = ctx_for_tenant(Uuid::new_v4());
    enable_flagging_backend(&svc, &ctx).await;
    let mut policy = TenantPolicy::default_for_tenant(ctx.subject_tenant_id());
    policy.enabled_backends = vec![FLAGGING_VENDOR.to_owned()];
    assert_eq!(
        prompt_scanner_sdk::PolicyProvider::get_policy(&*plugin, &ctx, ctx.subject_tenant_id())
            .await
            .unwrap(),
        Some(policy),
        "the write went to the plugin"
    );
    let verdict = svc.scan(&ctx, scan_request()).await.unwrap();
    assert_eq!(verdict.action, VerdictAction::Block);
}

/// What `fail_mode` does with each way the policy can go unresolved.
#[tokio::test]
async fn policy_resolution_failures_follow_fail_mode() {
    #[derive(Clone, Copy, Debug)]
    enum Broken {
        MalformedInstance,
        NoClient,
        NothingConfigured,
    }
    for broken in [
        Broken::MalformedInstance,
        Broken::NoClient,
        Broken::NothingConfigured,
    ] {
        for mode in [FailMode::Closed, FailMode::Open] {
            let hub = Arc::new(ClientHub::new());
            let mut instances = Vec::new();
            match broken {
                Broken::MalformedInstance => {
                    let json = register_provider(&hub, &mut instances, None);
                    let mut bad = json;
                    bad["priority"] = serde_json::json!("high");
                    instances.pop();
                    instances.push(bad);
                }
                Broken::NoClient => {
                    register_provider(&hub, &mut instances, None);
                }
                Broken::NothingConfigured => {}
            }
            register_registry(&hub, instances);
            let mut wiring = Wiring::new(hub).await;
            wiring.provider = None;
            wiring.settings.fail_mode = mode;
            let svc = wiring.build();
            let ctx = ctx_for_tenant(Uuid::new_v4());

            let result = svc.scan(&ctx, scan_request()).await;
            match mode {
                FailMode::Closed => assert!(
                    matches!(result, Err(DomainError::PolicyUnavailable { .. })),
                    "{broken:?}: {result:?}"
                ),
                FailMode::Open => assert_eq!(
                    result.unwrap().action,
                    VerdictAction::Allow,
                    "{broken:?}: the permissive default"
                ),
            }
        }
    }
}

#[tokio::test]
async fn the_read_only_fallback_refuses_writes_as_unsupported() {
    let mut wiring = Wiring::new(hub_with(Vec::new())).await;
    wiring.provider = Some(Arc::new(ConfigPolicyProvider::new(Some(
        TenantPolicy::default_for_tenant(Uuid::nil()),
    ))));
    let svc = wiring.build();
    let ctx = ctx_for_tenant(Uuid::new_v4());
    assert!(matches!(
        svc.put_policy(&ctx, TenantPolicy::default_for_tenant(Uuid::nil()))
            .await,
        Err(DomainError::Unsupported { .. })
    ));

    let mut open = Wiring::new(hub_with(Vec::new())).await;
    open.provider = None;
    open.settings.fail_mode = FailMode::Open;
    let svc = open.build();
    assert!(matches!(
        svc.put_policy(&ctx, TenantPolicy::default_for_tenant(Uuid::nil()))
            .await,
        Err(DomainError::Unsupported { .. })
    ));
}

#[tokio::test]
async fn list_backend_instances_reports_availability() {
    let hub = Arc::new(ClientHub::new());
    let mut instances = Vec::new();
    register_backend(&hub, &mut instances, FLAGGING_VENDOR, flagging());
    let (orphan, json) = PluginV1::<prompt_scanner_sdk::ScannerBackendSpecV1>::build_registration(
        "cf.test.orphan.detector.v1",
        "orphan",
        5,
    )
    .unwrap();
    instances.push(json);
    register_registry(&hub, instances);
    let svc = make_service(hub).await;

    let mut listed = svc
        .list_backend_instances(&ctx_for_tenant(Uuid::new_v4()))
        .await
        .unwrap();
    listed.sort_by(|a, b| a.vendor.cmp(&b.vendor));
    assert_eq!(listed.len(), 2);
    assert!(listed[0].available, "{:?}", listed[0]);
    assert_eq!(listed[1].gts_id, orphan.to_string());
    assert!(!listed[1].available);
    assert_eq!(listed[1].priority, 5);
}

// ── Attachments ──────────────────────────────────────────────────────────

fn upload(text: &str) -> ScanRequest {
    ScanRequest::file(
        "test-connector",
        Attachment::new(text.as_bytes().to_vec())
            .with_filename("note.txt")
            .with_mime_type("text/plain"),
    )
}

#[tokio::test]
async fn an_attachment_is_scanned_as_the_text_file_parser_extracts() {
    let hub = hub_with_flagging_backend();
    register_file_parser(&hub, FakeFileParser::Text);
    let svc = make_service(hub).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(&svc, &ctx, &[FLAGGING_VENDOR], |p| {
        p.evidence.enabled = true;
    })
    .await;

    let verdict = svc
        .scan(&ctx, upload("contact me at leaked@example.com"))
        .await
        .unwrap();
    assert_eq!(verdict.action, VerdictAction::Block);
    svc.wait_for_pending_writes().await;
    let rows = stored_rows(&svc, ctx.subject_tenant_id(), verdict.trace_id).await;
    assert!(
        rows[0].excerpt.as_ref().unwrap().text.starts_with("conta"),
        "the excerpt comes from the extracted text"
    );
}

#[tokio::test]
async fn attachment_failures_map_to_their_causes() {
    let ctx = ctx_for_tenant(Uuid::new_v4());
    let cases: Vec<(FakeFileParser, &str, ErrorCheck)> = vec![
        (
            FakeFileParser::Hang,
            "x",
            |e| matches!(e, DomainError::BackendTimeout { backend } if backend == "file-parser"),
        ),
        (
            FakeFileParser::Fail(FileParserClientError::Internal("down".to_owned())),
            "x",
            |e| matches!(e, DomainError::Internal(_)),
        ),
        (
            FakeFileParser::Fail(FileParserClientError::ParseFailed {
                message: "corrupt".to_owned(),
            }),
            "x",
            |e| matches!(e, DomainError::AttachmentParsingFailed { .. }),
        ),
        (FakeFileParser::Text, "", |e| {
            matches!(e, DomainError::Validation { .. })
        }),
    ];
    for (parser, text, expected) in cases {
        let hub = hub_with_flagging_backend();
        register_file_parser(&hub, parser);
        let svc = make_service(hub).await;
        // An empty attachment is refused by request validation, before
        // file-parser is called.
        let req = if text.is_empty() {
            ScanRequest::file("c", Attachment::new(Vec::new()))
        } else {
            upload(text)
        };
        let err = svc.scan(&ctx, req).await.expect_err("must fail");
        assert!(expected(&err), "{err:?}");
    }
}

#[tokio::test]
async fn extracted_text_over_the_payload_limit_is_refused() {
    let hub = hub_with_flagging_backend();
    register_file_parser(&hub, FakeFileParser::Text);
    let svc = make_service(hub).await;
    let big = "a".repeat(prompt_scanner_sdk::MAX_PAYLOAD_BYTES + 1);
    let err = svc
        .scan(&ctx_for_tenant(Uuid::new_v4()), upload(&big))
        .await
        .expect_err("too big");
    assert!(matches!(err, DomainError::Validation { ref field, .. } if field == "attachment"));
}

/// A file is only accepted with `ContentKind::Upload`.
#[tokio::test]
async fn attachment_on_non_upload_kind_is_rejected() {
    let svc = make_service(Arc::new(ClientHub::new())).await;
    let mut req = upload("x");
    req.content_kind = ContentKind::Prompt;
    assert!(matches!(
        svc.scan(&ctx_for_tenant(Uuid::new_v4()), req).await,
        Err(DomainError::Validation { .. })
    ));
}

// ── Async jobs ───────────────────────────────────────────────────────────

#[tokio::test]
async fn scan_async_completes_via_job_worker() {
    let svc = make_service(hub_with_flagging_backend()).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    enable_flagging_backend(&svc, &ctx).await;

    let handle = svc.scan_async(&ctx, scan_request()).await.unwrap();
    assert_eq!(handle.status, ScanJobStatus::Queued);
    assert_eq!(
        svc.get_job_status(&ctx, handle.job_id).await.unwrap(),
        ScanJobStatus::Queued
    );

    assert!(svc.process_next_job().await);
    assert_eq!(
        svc.get_job_status(&ctx, handle.job_id).await.unwrap(),
        ScanJobStatus::Completed {
            trace_id: handle.trace_id
        }
    );
    assert!(matches!(
        svc.get_job_status(&ctx_for_tenant(Uuid::new_v4()), handle.job_id)
            .await,
        Err(DomainError::JobNotFound { .. })
    ));

    svc.wait_for_pending_writes().await;
    let verdict = svc.get_verdict(&ctx, handle.trace_id).await.unwrap();
    assert_eq!(verdict.trace_id, handle.trace_id);
}

#[tokio::test]
async fn scan_async_accepts_an_attachment() {
    let hub = hub_with_flagging_backend();
    register_file_parser(&hub, FakeFileParser::Text);
    let svc = make_service(hub).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    enable_flagging_backend(&svc, &ctx).await;

    let handle = svc.scan_async(&ctx, upload("secret text")).await.unwrap();
    assert!(svc.process_next_job().await);
    assert_eq!(
        svc.get_job_status(&ctx, handle.job_id).await.unwrap(),
        ScanJobStatus::Completed {
            trace_id: handle.trace_id
        }
    );
}

#[tokio::test]
async fn a_failed_async_job_reports_a_stable_code() {
    let svc = make_service(hub_with(vec![("slow_vendor", Arc::new(SlowBackend))])).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(&svc, &ctx, &["slow_vendor"], |p| {
        p.required_backends = vec!["slow_vendor".to_owned()];
    })
    .await;

    let handle = svc.scan_async(&ctx, scan_request()).await.unwrap();
    assert!(svc.process_next_job().await);
    assert_eq!(
        svc.get_job_status(&ctx, handle.job_id).await.unwrap(),
        ScanJobStatus::Failed {
            code: ScanJobFailureCode::RequiredBackendFailed
        }
    );
}

#[tokio::test]
async fn a_full_job_queue_refuses_scan_async_as_overloaded() {
    let metrics = Arc::new(CountingMetrics::default());
    let mut wiring = Wiring::new(hub_with(Vec::new())).await;
    wiring.settings.job_queue_capacity = 1;
    wiring.metrics = Arc::clone(&metrics) as Arc<dyn EvidenceMetricsPort>;
    let svc = wiring.build();
    let ctx = ctx_for_tenant(Uuid::new_v4());

    svc.scan_async(&ctx, scan_request()).await.unwrap();
    assert!(matches!(
        svc.scan_async(&ctx, scan_request()).await,
        Err(DomainError::Overloaded)
    ));
    assert_eq!(metrics.jobs_dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn jobs_still_queued_at_shutdown_are_dropped_counted_and_failed() {
    let metrics = Arc::new(CountingMetrics::default());
    let mut wiring = Wiring::new(hub_with(Vec::new())).await;
    wiring.metrics = Arc::clone(&metrics) as Arc<dyn EvidenceMetricsPort>;
    let svc = wiring.build();
    let ctx = ctx_for_tenant(Uuid::new_v4());
    let a = svc.scan_async(&ctx, scan_request()).await.unwrap();
    svc.scan_async(&ctx, scan_request()).await.unwrap();

    assert_eq!(svc.drop_queued_jobs().await, 2);
    assert_eq!(metrics.jobs_dropped.load(Ordering::SeqCst), 2);
    assert_eq!(
        svc.get_job_status(&ctx, a.job_id).await.unwrap(),
        ScanJobStatus::Failed {
            code: ScanJobFailureCode::Internal
        }
    );
    assert!(matches!(
        svc.scan_async(&ctx, scan_request()).await,
        Err(DomainError::Internal(_))
    ));
}

#[tokio::test]
async fn a_cancelled_drain_fails_the_running_job() {
    let svc = make_service(hub_with(vec![("slow_vendor", Arc::new(SlowBackend))])).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(&svc, &ctx, &["slow_vendor"], |_| {}).await;
    let handle = svc.scan_async(&ctx, scan_request()).await.unwrap();

    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.cancel();
    });
    // The backend timeout (200 ms) is longer than the cancellation (50 ms).
    assert_eq!(svc.drain_jobs(10, &cancel).await, 1);
    assert_eq!(
        svc.get_job_status(&ctx, handle.job_id).await.unwrap(),
        ScanJobStatus::Failed {
            code: ScanJobFailureCode::Internal
        }
    );
}

/// The worker loop runs queued jobs on its own ticks; a finished job's
/// status is forgotten after `job_status_ttl`.
#[tokio::test(flavor = "current_thread")]
async fn the_job_loop_runs_jobs_and_statuses_expire() {
    let mut wiring = Wiring::new(hub_with(Vec::new())).await;
    wiring.settings.job_status_ttl = Duration::from_secs(60);
    let svc = Arc::new(wiring.build());
    let ctx = ctx_for_tenant(Uuid::new_v4());
    tokio::time::pause();

    let cancel = CancellationToken::new();
    let worker = tokio::spawn(crate::gear::run_job_loop(
        Arc::clone(&svc),
        cancel.clone(),
        Duration::from_millis(100),
        8,
    ));
    let handle = svc.scan_async(&ctx, scan_request()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(matches!(
        svc.get_job_status(&ctx, handle.job_id).await.unwrap(),
        ScanJobStatus::Completed { .. }
    ));

    tokio::time::sleep(Duration::from_secs(61)).await;
    assert!(matches!(
        svc.get_job_status(&ctx, handle.job_id).await,
        Err(DomainError::JobNotFound { .. })
    ));

    cancel.cancel();
    worker.await.unwrap();
}

// ── Deferred backends ────────────────────────────────────────────────────

/// Through the public API: a fast backend's finding lands in the synchronous
/// verdict, a deferring backend's only after the worker runs it, as a new
/// row. Rows are never updated: the first keeps its verdict, the appended
/// one carries the re-aggregated verdict, and a read takes the highest.
#[tokio::test]
async fn deferred_backend_finding_is_appended_to_the_scan() {
    // Defers every inline run and only answers on the deferred run, like a
    // backend that defers by input size: the retry must be marked.
    let deferring = Arc::new(DeferInlineBackend::default());
    let svc = make_service(hub_with(vec![
        (
            "fast_vendor",
            Arc::new(FakeBackend {
                category: "pii.email".to_owned(),
                action: VerdictAction::Audit,
            }),
        ),
        ("deferring_vendor", Arc::clone(&deferring) as _),
    ]))
    .await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(&svc, &ctx, &["fast_vendor", "deferring_vendor"], |_| {}).await;

    let verdict = svc.scan(&ctx, scan_request()).await.unwrap();
    assert_eq!(verdict.findings.len(), 1);
    assert_eq!(verdict.action, VerdictAction::Audit);
    assert_eq!(
        verdict.unscored_backends,
        [UnscoredBackend::new(
            "deferring_vendor",
            UnscoredReason::Deferred
        )]
    );

    assert!(
        svc.process_next_job().await,
        "the deferred backend was queued"
    );
    svc.wait_for_pending_writes().await;

    let merged = svc.get_verdict(&ctx, verdict.trace_id).await.unwrap();
    assert_eq!(merged.findings.len(), 2);
    assert_eq!(merged.action, VerdictAction::Block);
    let rows = stored_rows(&svc, ctx.subject_tenant_id(), verdict.trace_id).await;
    let shape: Vec<(u8, VerdictAction)> = rows.iter().map(|r| (r.ordinal, r.action)).collect();
    assert_eq!(
        shape,
        [(0, VerdictAction::Audit), (1, VerdictAction::Block)]
    );
    assert_eq!(rows[1].finding.category, "pii.person");
    assert_eq!(deferring.phases(), [ScanPhase::Inline, ScanPhase::Deferred]);
}

/// Inside a `scan_async` job the deferred run happens right away, marked
/// as such, and its findings are stored.
#[tokio::test]
async fn deferred_backend_findings_are_recorded_by_an_async_job() {
    let deferring = Arc::new(DeferInlineBackend::default());
    let svc = make_service(hub_with(vec![(
        "deferring_vendor",
        Arc::clone(&deferring) as _,
    )]))
    .await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(&svc, &ctx, &["deferring_vendor"], |p| {
        p.required_backends = vec!["deferring_vendor".to_owned()];
    })
    .await;

    let handle = svc.scan_async(&ctx, scan_request()).await.unwrap();
    assert!(svc.process_next_job().await);
    assert_eq!(
        svc.get_job_status(&ctx, handle.job_id).await.unwrap(),
        ScanJobStatus::Completed {
            trace_id: handle.trace_id
        }
    );
    svc.wait_for_pending_writes().await;
    let stored = svc.get_verdict(&ctx, handle.trace_id).await.unwrap();
    assert_eq!(stored.action, VerdictAction::Block);
    assert_eq!(stored.findings.len(), 1);
    assert_eq!(stored.findings[0].category, "pii.person");
    assert_eq!(deferring.phases(), [ScanPhase::Inline, ScanPhase::Deferred]);
}

/// A required backend that defers again inside a `scan_async` job fails the
/// job; on the synchronous path the repeat is counted.
#[tokio::test]
async fn a_required_backend_that_defers_again_fails_the_async_job() {
    let metrics = Arc::new(CountingMetrics::default());
    let mut wiring = Wiring::new(hub_with(vec![(
        "deferring_vendor",
        Arc::new(DeferInlineBackend {
            always_defer: true,
            ..DeferInlineBackend::default()
        }),
    )]))
    .await;
    wiring.metrics = Arc::clone(&metrics) as Arc<dyn EvidenceMetricsPort>;
    let svc = wiring.build();
    let ctx = ctx_for_tenant(Uuid::new_v4());
    put_policy_with(&svc, &ctx, &["deferring_vendor"], |p| {
        p.required_backends = vec!["deferring_vendor".to_owned()];
    })
    .await;

    let handle = svc.scan_async(&ctx, scan_request()).await.unwrap();
    assert!(svc.process_next_job().await);
    assert_eq!(
        svc.get_job_status(&ctx, handle.job_id).await.unwrap(),
        ScanJobStatus::Failed {
            code: ScanJobFailureCode::RequiredBackendFailed
        }
    );
    assert_eq!(metrics.required_deferred_failures.load(Ordering::SeqCst), 1);

    svc.scan(&ctx, scan_request()).await.unwrap();
    assert!(svc.process_next_job().await);
    assert_eq!(metrics.required_deferred_failures.load(Ordering::SeqCst), 2);
}

// ── Storage: the `evidences` table ───────────────────────────────────────

#[tokio::test]
async fn a_flagged_scan_stores_one_row_per_finding_without_content_by_default() {
    let svc = make_service(hub_with_flagging_backend()).await;
    let tenant_id = Uuid::new_v4();
    let ctx = ctx_for_tenant(tenant_id);
    put_policy_with(&svc, &ctx, &[FLAGGING_VENDOR], |p| {
        p.settings_schema = Some("1.0".to_owned());
    })
    .await;

    let mut req = scan_request();
    req.correlation_id = Some("req-42".to_owned());
    let verdict = svc.scan(&ctx, req).await.unwrap();
    svc.wait_for_pending_writes().await;

    let rows = stored_rows(&svc, tenant_id, verdict.trace_id).await;
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.ordinal, 0);
    assert_eq!(row.tenant_id, tenant_id);
    assert_eq!(row.correlation_id.as_deref(), Some("req-42"));
    assert_eq!(row.connector_id, "test-connector");
    assert_eq!(row.action, VerdictAction::Block);
    assert_eq!(row.settings_schema.as_deref(), Some("1.0"));
    assert_eq!(row.finding.detector_version.as_deref(), Some(FAKE_VERSION));
    assert!(row.excerpt.is_none(), "evidence is off by default");
    assert_eq!(row.expires_at, row.created_at + chrono::TimeDelta::days(90));

    let read = svc.get_verdict(&ctx, verdict.trace_id).await.unwrap();
    assert_eq!(read.action, VerdictAction::Block);
    assert_eq!(read.findings, verdict.findings);
}

#[tokio::test]
async fn evidence_stores_the_text_around_a_finding_for_at_most_30_days() {
    let svc = make_service(hub_with_flagging_backend()).await;
    let tenant_id = Uuid::new_v4();
    let ctx = ctx_for_tenant(tenant_id);
    put_policy_with(&svc, &ctx, &[FLAGGING_VENDOR], |p| {
        p.retention = Some(Duration::from_hours(60 * 24));
        p.evidence.enabled = true;
    })
    .await;

    let verdict = svc.scan(&ctx, scan_request()).await.unwrap();
    svc.wait_for_pending_writes().await;

    let rows = stored_rows(&svc, tenant_id, verdict.trace_id).await;
    let excerpt = rows[0].excerpt.as_ref().expect("evidence enabled");
    assert_eq!(
        excerpt.text, "hello world",
        "a short prompt is stored whole"
    );
    assert_eq!(
        rows[0].expires_at,
        rows[0].created_at + chrono::TimeDelta::days(30),
        "60 days requested, clamped to max_evidence_retention"
    );
}

#[tokio::test]
async fn correlation_lookup_finds_the_callers_scans_only() {
    let svc = make_service(hub_with_flagging_backend()).await;
    let ctx_a = ctx_for_tenant(Uuid::new_v4());
    let ctx_b = ctx_for_tenant(Uuid::new_v4());
    enable_flagging_backend(&svc, &ctx_a).await;
    enable_flagging_backend(&svc, &ctx_b).await;

    let mut req = scan_request();
    req.correlation_id = Some("shared-id".to_owned());
    let first = svc.scan(&ctx_a, req.clone()).await.unwrap();
    let second = svc.scan(&ctx_a, req.clone()).await.unwrap();
    svc.scan(&ctx_b, req).await.unwrap();
    svc.wait_for_pending_writes().await;

    let ids: Vec<Uuid> = svc
        .find_by_correlation_id(&ctx_a, "shared-id")
        .await
        .unwrap()
        .iter()
        .map(|v| v.trace_id)
        .collect();
    assert_eq!(
        ids,
        [second.trace_id, first.trace_id],
        "newest first, own tenant only"
    );
    assert!(
        svc.find_by_correlation_id(&ctx_a, "unknown")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn evidence_removal_is_a_hard_delete_scoped_to_the_tenant() {
    let svc = make_service(hub_with_flagging_backend()).await;
    let tenant_a = Uuid::new_v4();
    let ctx_a = ctx_for_tenant(tenant_a);
    let ctx_b = ctx_for_tenant(Uuid::new_v4());
    enable_flagging_backend(&svc, &ctx_a).await;
    let verdict = svc.scan(&ctx_a, scan_request()).await.unwrap();
    svc.wait_for_pending_writes().await;

    assert!(matches!(
        svc.delete_evidence(&ctx_b, verdict.trace_id).await,
        Err(DomainError::VerdictNotFound { .. })
    ));
    assert_eq!(
        svc.delete_evidence(&ctx_a, verdict.trace_id).await.unwrap(),
        1
    );
    assert!(
        stored_rows(&svc, tenant_a, verdict.trace_id)
            .await
            .is_empty()
    );
    assert!(matches!(
        svc.delete_evidence(&ctx_a, verdict.trace_id).await,
        Err(DomainError::VerdictNotFound { .. })
    ));
}

/// `count` rows for `tenant_id`, expired when `expired`, written straight to
/// the table.
async fn seed_rows(svc: &TestService, tenant_id: Uuid, count: u8, expired: bool) {
    let now = chrono::Utc::now();
    let (created_at, expires_at) = if expired {
        (
            now - chrono::TimeDelta::days(100),
            now - chrono::TimeDelta::days(10),
        )
    } else {
        (now, now + chrono::TimeDelta::days(10))
    };
    let rows = (0..count)
        .map(|ordinal| EvidenceRow {
            trace_id: Uuid::now_v7(),
            ordinal,
            tenant_id,
            correlation_id: None,
            connector_id: "c".to_owned(),
            content_kind: ContentKind::Prompt,
            created_at,
            expires_at,
            action: VerdictAction::Block,
            settings_schema: None,
            finding: prompt_scanner_sdk::Finding::from_detection(
                prompt_scanner_sdk::Detection::new(
                    "pii.email",
                    prompt_scanner_sdk::Severity::Low,
                    0.5,
                    VerdictAction::Block,
                ),
                "gts.x",
                None,
            ),
            excerpt: None,
        })
        .collect();
    let conn = svc.db.conn().unwrap();
    svc.repo
        .insert_all(&conn, &AccessScope::for_tenant(tenant_id), rows)
        .await
        .unwrap();
}

fn maintenance(
    svc: &TestService,
    metrics: Arc<dyn EvidenceMetricsPort>,
    batch: u64,
) -> EvidenceMaintenanceJobs<OrmEvidenceRepository> {
    EvidenceMaintenanceJobs::new(
        Arc::clone(&svc.db),
        Arc::new(OrmEvidenceRepository::new()),
        metrics,
        batch,
    )
}

async fn count_rows(svc: &TestService) -> u64 {
    use sea_orm::EntityTrait as _;
    use toolkit_db::secure::SecureEntityExt as _;
    let conn = svc.db.conn().unwrap();
    crate::infra::storage::entity::evidence::Entity::find()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .count(&conn)
        .await
        .unwrap()
}

#[tokio::test]
async fn retention_deletes_expired_rows_in_batches_and_reports_the_backlog() {
    let svc = make_service(hub_with(Vec::new())).await;
    let tenant = Uuid::new_v4();
    seed_rows(&svc, tenant, 5, true).await;
    seed_rows(&svc, tenant, 2, false).await;
    let metrics = Arc::new(CountingMetrics::default());
    let jobs = maintenance(
        &svc,
        Arc::clone(&metrics) as Arc<dyn EvidenceMetricsPort>,
        2,
    );

    // Cancelled before the first batch: nothing deleted, the backlog counted.
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(jobs.run_retention(&cancelled).await.unwrap(), Some(0));
    assert_eq!(metrics.backlog.load(Ordering::SeqCst), 5);

    assert_eq!(
        jobs.run_retention(&CancellationToken::new()).await.unwrap(),
        Some(5),
        "three batches of at most two"
    );
    assert_eq!(metrics.deleted.load(Ordering::SeqCst), 5);
    assert_eq!(metrics.backlog.load(Ordering::SeqCst), 0);
    assert_eq!(count_rows(&svc).await, 2, "live rows stay");
}

#[tokio::test]
async fn expired_rows_are_invisible_before_retention_runs() {
    let svc = make_service(hub_with_flagging_backend()).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    enable_flagging_backend(&svc, &ctx).await;
    let live = svc.scan(&ctx, scan_request()).await.unwrap();
    svc.wait_for_pending_writes().await;
    let mut expired = stored_rows(&svc, ctx.subject_tenant_id(), live.trace_id)
        .await
        .remove(0);
    expired.trace_id = Uuid::now_v7();
    expired.created_at -= chrono::TimeDelta::days(100);
    expired.expires_at = expired.created_at + chrono::TimeDelta::days(90);
    let conn = svc.db.conn().unwrap();
    svc.repo
        .insert_all(
            &conn,
            &AccessScope::for_tenant(ctx.subject_tenant_id()),
            vec![expired.clone()],
        )
        .await
        .unwrap();
    assert!(matches!(
        svc.get_verdict(&ctx, expired.trace_id).await,
        Err(DomainError::VerdictNotFound { .. })
    ));
}

#[tokio::test]
async fn tenant_deletion_removes_only_that_tenants_rows_in_batches_and_is_idempotent() {
    let svc = make_service(hub_with(Vec::new())).await;
    let (tenant_a, tenant_b) = (Uuid::new_v4(), Uuid::new_v4());
    seed_rows(&svc, tenant_a, 5, false).await;
    seed_rows(&svc, tenant_b, 1, false).await;
    let jobs = maintenance(&svc, Arc::new(CountingMetrics::default()), 2);
    let cancel = CancellationToken::new();

    assert_eq!(
        jobs.delete_tenant_evidence(tenant_a, &cancel)
            .await
            .unwrap(),
        5
    );
    assert_eq!(
        jobs.delete_tenant_evidence(tenant_a, &cancel)
            .await
            .unwrap(),
        0
    );
    assert_eq!(count_rows(&svc).await, 1, "the other tenant's row stays");
}

/// The maintenance delete takes at most `limit` rows per statement.
#[tokio::test]
async fn a_maintenance_batch_is_bounded_by_its_limit() {
    use crate::domain::repos::EvidenceMaintenance;
    let svc = make_service(hub_with(Vec::new())).await;
    seed_rows(&svc, Uuid::new_v4(), 5, true).await;
    let conn = svc.db.conn().unwrap();
    let repo = OrmEvidenceRepository::new();
    let deleted = repo
        .delete_expired(&conn, &AccessScope::allow_all(), chrono::Utc::now(), 2)
        .await
        .unwrap();
    assert_eq!(deleted, 2);
    assert_eq!(count_rows(&svc).await, 3);
}

/// A repository whose inserts always fail: a database outage.
struct FailingInserts;

#[async_trait::async_trait]
impl EvidenceRepository for FailingInserts {
    async fn insert_all<C: toolkit_db::secure::DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _rows: Vec<EvidenceRow>,
    ) -> Result<(), DomainError> {
        Err(DomainError::database(
            "insert",
            std::io::Error::other("outage"),
        ))
    }
    async fn find_by_trace<C: toolkit_db::secure::DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _trace_id: Uuid,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<EvidenceRow>, DomainError> {
        Ok(Vec::new())
    }
    async fn find_by_correlation<C: toolkit_db::secure::DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _correlation_id: &str,
        _now: chrono::DateTime<chrono::Utc>,
        _limit: u64,
    ) -> Result<Vec<EvidenceRow>, DomainError> {
        Ok(Vec::new())
    }
    async fn delete_by_trace<C: toolkit_db::secure::DBRunner>(
        &self,
        _runner: &C,
        _scope: &AccessScope,
        _trace_id: Uuid,
    ) -> Result<u64, DomainError> {
        Ok(0)
    }
}

/// The verdict goes first: a failed insert loses the evidence and is
/// counted, but the scan still returns its verdict.
#[tokio::test]
async fn a_database_outage_never_fails_the_scan() {
    let metrics = Arc::new(CountingMetrics::default());
    let mut wiring = Wiring::new(hub_with_flagging_backend()).await;
    wiring.metrics = Arc::clone(&metrics) as Arc<dyn EvidenceMetricsPort>;
    let svc = wiring.with_repo(Arc::new(FailingInserts)).build();
    let ctx = ctx_for_tenant(Uuid::new_v4());
    enable_flagging_backend(&svc, &ctx).await;

    let verdict = svc.scan(&ctx, scan_request()).await.unwrap();
    assert_eq!(verdict.action, VerdictAction::Block);
    svc.wait_for_pending_writes().await;
    assert_eq!(metrics.write_failures.load(Ordering::SeqCst), 1);
}

/// A full write queue drops the scan's rows, counted; the scan still
/// succeeds.
#[tokio::test]
async fn a_full_write_queue_drops_and_counts_rows() {
    let metrics = Arc::new(CountingMetrics::default());
    let mut settings = test_settings();
    settings.evidence.write_queue_capacity = 1;
    // The writer is kept but not run, so the queue fills.
    let (svc, _writer) = Service::new(
        in_memory_db().await,
        hub_with_flagging_backend(),
        Arc::new(OrmEvidenceRepository::new()),
        Some(Arc::new(InMemoryPolicyProvider::default())),
        mock_enforcer(),
        Arc::clone(&metrics) as Arc<dyn EvidenceMetricsPort>,
        settings,
    );
    let ctx = ctx_for_tenant(Uuid::new_v4());
    enable_flagging_backend(&svc, &ctx).await;

    svc.scan(&ctx, scan_request()).await.unwrap();
    svc.scan(&ctx, scan_request()).await.unwrap();
    assert_eq!(metrics.queue_full.load(Ordering::SeqCst), 1);
}
