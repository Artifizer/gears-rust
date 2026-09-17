//! Axum handlers for the prompt-scanner REST API.

use std::sync::Arc;

use axum::Extension;
use axum::extract::{Path, Query};
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use uuid::Uuid;

use prompt_scanner_sdk::ScanRequest;
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;

use crate::api::rest::dto::{
    BackendInstanceDto, CorrelationQuery, EvidenceDeletedDto, ScanJobHandleDto,
    ScanJobStatusResponseDto, ScanRequestDto, ScanVerdictDto, TenantPolicyDto,
    TenantPolicyUpdateDto,
};
use crate::gear::ConcreteService;

/// `POST /prompt-scanner/v1/scans` — synchronous scan.
pub async fn create_scan(
    uri: Uri,
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(req_body): Json<ScanRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let verdict = svc.scan(&ctx, ScanRequest::try_from(req_body)?).await?;
    let trace_id = verdict.trace_id.to_string();
    Ok(created_json(ScanVerdictDto::from(verdict), &uri, &trace_id).into_response())
}

/// `POST /prompt-scanner/v1/scans/async` — queue a scan for async
/// processing. `202 Accepted` with the job handle.
pub async fn create_scan_async(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(req_body): Json<ScanRequestDto>,
) -> ApiResult<impl IntoResponse> {
    let handle = svc
        .scan_async(&ctx, ScanRequest::try_from(req_body)?)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(ScanJobHandleDto::from(handle))))
}

/// `GET /prompt-scanner/v1/scans/async/{job_id}` — an async job's status.
pub async fn get_scan_job(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(job_id): Path<Uuid>,
) -> ApiResult<JsonBody<ScanJobStatusResponseDto>> {
    let status = svc.get_job_status(&ctx, job_id).await?;
    Ok(Json(ScanJobStatusResponseDto::new(job_id, status)))
}

/// `GET /prompt-scanner/v1/scans/{trace_id}` — a flagged scan's stored findings.
pub async fn get_scan(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(trace_id): Path<Uuid>,
) -> ApiResult<JsonBody<ScanVerdictDto>> {
    let verdict = svc.get_verdict(&ctx, trace_id).await?;
    Ok(Json(verdict.into()))
}

/// `DELETE /prompt-scanner/v1/scans/{trace_id}` — manual evidence removal.
pub async fn delete_scan_evidence(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(trace_id): Path<Uuid>,
) -> ApiResult<JsonBody<EvidenceDeletedDto>> {
    let deleted = svc.delete_evidence(&ctx, trace_id).await?;
    Ok(Json(EvidenceDeletedDto { trace_id, deleted }))
}

/// `GET /prompt-scanner/v1/scans?correlation_id=...` — correlation lookup.
pub async fn find_scans_by_correlation_id(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Query(query): Query<CorrelationQuery>,
) -> ApiResult<JsonBody<Vec<ScanVerdictDto>>> {
    let verdicts = svc
        .find_by_correlation_id(&ctx, &query.correlation_id)
        .await?;
    Ok(Json(verdicts.into_iter().map(Into::into).collect()))
}

/// `GET /prompt-scanner/v1/policy` — get the calling tenant's backend policy.
pub async fn get_policy(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
) -> ApiResult<JsonBody<TenantPolicyDto>> {
    let policy = svc.get_policy(&ctx).await?;
    Ok(Json(policy.into()))
}

/// `PUT /prompt-scanner/v1/policy` — update the calling tenant's backend policy.
pub async fn put_policy(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(req_body): Json<TenantPolicyUpdateDto>,
) -> ApiResult<JsonBody<TenantPolicyDto>> {
    let policy = svc.put_policy(&ctx, req_body.into()).await?;
    Ok(Json(policy.into()))
}

/// `GET /prompt-scanner/v1/backends` — list registered scanner backend
/// plugin instances and their availability.
pub async fn list_backends(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
) -> ApiResult<JsonBody<Vec<BackendInstanceDto>>> {
    let instances = svc.list_backend_instances(&ctx).await?;
    let dtos = instances
        .into_iter()
        .map(|i| BackendInstanceDto {
            gts_id: i.gts_id,
            vendor: i.vendor,
            priority: i.priority,
            available: i.available,
        })
        .collect();
    Ok(Json(dtos))
}
