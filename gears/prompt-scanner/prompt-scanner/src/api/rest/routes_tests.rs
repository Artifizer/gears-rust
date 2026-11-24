#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: a failed expectation is the test failing"
)]

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use crate::domain::test_support::{DenyActions, FakeBackend, Wiring, ctx_for_tenant, hub_with};

use super::register_routes;

const VENDOR: &str = "flagging_vendor";

async fn router_with(
    adjust: impl FnOnce(&mut Wiring<crate::infra::storage::OrmEvidenceRepository>),
) -> Router {
    let hub = hub_with(vec![(
        VENDOR,
        Arc::new(FakeBackend {
            category: "pii.email".to_owned(),
            action: prompt_scanner_sdk::VerdictAction::Block,
        }),
    )]);
    let mut wiring = Wiring::new(hub).await;
    adjust(&mut wiring);
    let svc = Arc::new(wiring.build());
    let openapi = OpenApiRegistryImpl::new();
    register_routes(Router::new(), &openapi, svc)
}

async fn router() -> Router {
    router_with(|_| {}).await
}

fn request(
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    ctx: &SecurityContext,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let body = body.map_or_else(Body::empty, |json| {
        Body::from(serde_json::to_vec(&json).unwrap())
    });
    let mut req = builder.body(body).unwrap();
    req.extensions_mut().insert(ctx.clone());
    req
}

async fn send(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    ctx: &SecurityContext,
) -> (StatusCode, serde_json::Value) {
    let resp = router
        .clone()
        .oneshot(request(method, uri, body, ctx))
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

fn scan_body() -> serde_json::Value {
    serde_json::json!({
        "connector_id": "test",
        "content_kind": "prompt",
        "payload": "hello world",
    })
}

async fn enable(router: &Router, ctx: &SecurityContext) {
    let (status, body) = send(
        router,
        "PUT",
        "/prompt-scanner/v1/policy",
        Some(serde_json::json!({"enabled_backends": [VENDOR]})),
        ctx,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["tenant_id"], ctx.subject_tenant_id().to_string());
    assert_eq!(body["default_action"], "allow");
}

#[tokio::test]
async fn a_scan_is_created_read_back_and_deleted() {
    let router = router().await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    enable(&router, &ctx).await;

    let (status, verdict) = send(
        &router,
        "POST",
        "/prompt-scanner/v1/scans",
        Some(scan_body()),
        &ctx,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{verdict}");
    assert_eq!(verdict["action"], "block");
    let trace_id = verdict["trace_id"].as_str().unwrap().to_owned();

    // The write is asynchronous; poll briefly.
    let uri = format!("/prompt-scanner/v1/scans/{trace_id}");
    let mut found = StatusCode::NOT_FOUND;
    for _ in 0..50 {
        found = send(&router, "GET", &uri, None, &ctx).await.0;
        if found == StatusCode::OK {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(found, StatusCode::OK);

    let (status, other) = send(&router, "GET", &uri, None, &ctx_for_tenant(Uuid::new_v4())).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{other}");

    let (status, deleted) = send(&router, "DELETE", &uri, None, &ctx).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["deleted"], 1);
}

#[tokio::test]
async fn scan_async_is_accepted_and_its_job_is_readable() {
    let router = router().await;
    let ctx = ctx_for_tenant(Uuid::new_v4());

    let (status, handle) = send(
        &router,
        "POST",
        "/prompt-scanner/v1/scans/async",
        Some(scan_body()),
        &ctx,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{handle}");
    assert_eq!(handle["status"], "queued");

    let uri = format!(
        "/prompt-scanner/v1/scans/async/{}",
        handle["job_id"].as_str().unwrap()
    );
    let (status, job) = send(&router, "GET", &uri, None, &ctx).await;
    assert_eq!(status, StatusCode::OK, "{job}");
    assert_eq!(job["status"], "queued");

    let (status, _) = send(&router, "GET", &uri, None, &ctx_for_tenant(Uuid::new_v4())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_full_queue_is_503() {
    let router = router_with(|w| w.settings.job_queue_capacity = 1).await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    let uri = "/prompt-scanner/v1/scans/async";
    assert_eq!(
        send(&router, "POST", uri, Some(scan_body()), &ctx).await.0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        send(&router, "POST", uri, Some(scan_body()), &ctx).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn caller_faults_are_4xx() {
    let router = router().await;
    let ctx = ctx_for_tenant(Uuid::new_v4());

    let mut empty = scan_body();
    empty["payload"] = serde_json::json!("");
    let (status, body) = send(
        &router,
        "POST",
        "/prompt-scanner/v1/scans",
        Some(empty),
        &ctx,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, _) = send(
        &router,
        "PUT",
        "/prompt-scanner/v1/policy",
        Some(serde_json::json!({"tenant_id": Uuid::new_v4()})),
        &ctx,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "another tenant's id");

    let (status, _) = send(
        &router,
        "GET",
        &format!("/prompt-scanner/v1/scans/{}", Uuid::now_v7()),
        None,
        &ctx,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(
        &router,
        "GET",
        "/prompt-scanner/v1/scans?correlation_id=",
        None,
        &ctx,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn backends_and_policy_are_readable() {
    let router = router().await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    let (status, backends) = send(&router, "GET", "/prompt-scanner/v1/backends", None, &ctx).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(backends[0]["vendor"], VENDOR);
    assert_eq!(backends[0]["available"], true);

    let (status, policy) = send(&router, "GET", "/prompt-scanner/v1/policy", None, &ctx).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(policy["default_action"], "allow");
}

#[tokio::test]
async fn a_pdp_deny_is_403_everywhere() {
    let router = router_with(|w| {
        w.enforcer = PolicyEnforcer::new(Arc::new(DenyActions(&[
            "create", "get", "delete", "update", "list",
        ])));
    })
    .await;
    let ctx = ctx_for_tenant(Uuid::new_v4());
    let id = Uuid::now_v7();
    let calls: Vec<(&str, String, Option<serde_json::Value>)> = vec![
        (
            "POST",
            "/prompt-scanner/v1/scans".to_owned(),
            Some(scan_body()),
        ),
        (
            "POST",
            "/prompt-scanner/v1/scans/async".to_owned(),
            Some(scan_body()),
        ),
        ("GET", format!("/prompt-scanner/v1/scans/async/{id}"), None),
        ("GET", format!("/prompt-scanner/v1/scans/{id}"), None),
        ("DELETE", format!("/prompt-scanner/v1/scans/{id}"), None),
        (
            "GET",
            "/prompt-scanner/v1/scans?correlation_id=x".to_owned(),
            None,
        ),
        ("GET", "/prompt-scanner/v1/policy".to_owned(), None),
        (
            "PUT",
            "/prompt-scanner/v1/policy".to_owned(),
            Some(serde_json::json!({})),
        ),
        ("GET", "/prompt-scanner/v1/backends".to_owned(), None),
    ];
    for (method, uri, body) in calls {
        let (status, problem) = send(&router, method, &uri, body, &ctx).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {problem}");
    }
}
