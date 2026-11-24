//! REST route registration for the prompt-scanner gear.
//!
//! Endpoints (`/prompt-scanner/v1/...`), per `docs/DESIGN.md`:
//!
//! | Method | Path              | Purpose                                  |
//! |--------|-------------------|-------------------------------------------|
//! | POST   | `/scans`          | Sync scan -> verdict                      |
//! | POST   | `/scans/async`    | Queue a scan -> job handle (202)          |
//! | GET    | `/scans/async/{job_id}` | An async job's status               |
//! | GET    | `/scans/{trace_id}` | A flagged scan's stored findings        |
//! | DELETE | `/scans/{trace_id}` | Remove a scan's evidence now            |
//! | GET    | `/scans?correlation_id=` | Correlation lookup                 |
//! | GET    | `/policy`         | Get tenant's backend policy                |
//! | PUT    | `/policy`         | Update tenant's backend policy             |
//! | GET    | `/backends`       | List registered scanner backend instances |

use std::sync::Arc;

use axum::Router;
use http::StatusCode;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::{
    CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature, OperationBuilder,
};

use crate::api::rest::{dto, handlers};
use crate::gear::ConcreteService;

const API_TAG: &str = "Scan Gateway";

pub(super) struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

/// Register all routes for the `prompt_scanner` gear.
pub(crate) fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteService>,
) -> Router {
    router = OperationBuilder::post("/prompt-scanner/v1/scans")
        .operation_id("prompt_scanner.create_scan")
        .summary("Synchronously scan a piece of content")
        .description(
            "Fans out to every enabled scanner backend for the tenant and aggregates findings \
             into a single verdict. A scan with findings is stored after the verdict returns.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::ScanRequestDto>(openapi, "Content to scan")
        .handler(handlers::create_scan)
        .json_response_with_schema::<dto::ScanVerdictDto>(
            openapi,
            StatusCode::CREATED,
            "Scan verdict",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .error_504(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/prompt-scanner/v1/scans/async")
        .operation_id("prompt_scanner.create_scan_async")
        .summary("Queue a scan for asynchronous processing")
        .description(
            "For large uploads. Returns a job handle (202); poll GET /scans/async/{job_id}, then \
             read a flagged scan's findings with GET /scans/{trace_id}. 503 when the job queue \
             is full.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::ScanRequestDto>(openapi, "Content to scan")
        .handler(handlers::create_scan_async)
        .json_response_with_schema::<dto::ScanJobHandleDto>(
            openapi,
            StatusCode::ACCEPTED,
            "Queued scan job handle",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/prompt-scanner/v1/scans/async/{job_id}")
        .operation_id("prompt_scanner.get_scan_job")
        .summary("Status of an async scan job")
        .description(
            "404 when the job is unknown, belongs to another tenant, or finished more than \
             `job_status_ttl` ago.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("job_id", "Job id from POST /scans/async")
        .handler(handlers::get_scan_job)
        .json_response_with_schema::<dto::ScanJobStatusResponseDto>(
            openapi,
            StatusCode::OK,
            "Job status",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/prompt-scanner/v1/scans/{trace_id}")
        .operation_id("prompt_scanner.get_scan")
        .summary("Fetch a flagged scan's stored findings by trace id")
        .description(
            "404 when the scan had no findings, is unknown, has expired or belongs to another \
             tenant; these cases are not told apart.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("trace_id", "Scan trace id")
        .handler(handlers::get_scan)
        .json_response_with_schema::<dto::ScanVerdictDto>(openapi, StatusCode::OK, "Scan verdict")
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/prompt-scanner/v1/scans/{trace_id}")
        .operation_id("prompt_scanner.delete_scan_evidence")
        .summary("Remove a scan's stored findings and evidence before they expire")
        .description("A hard delete. Returns the number of finding rows removed.")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .path_param("trace_id", "Scan trace id")
        .handler(handlers::delete_scan_evidence)
        .json_response_with_schema::<dto::EvidenceDeletedDto>(
            openapi,
            StatusCode::OK,
            "Rows removed",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/prompt-scanner/v1/scans")
        .operation_id("prompt_scanner.find_scans_by_correlation_id")
        .summary("Correlation lookup: the caller's stored scans for one correlation id")
        .description(
            "Newest first, at most 1,000 findings. Scans cannot be listed: `correlation_id` \
             is required.",
        )
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .query_param("correlation_id", true, "The consumer's request id")
        .handler(handlers::find_scans_by_correlation_id)
        .json_array_response_with_schema::<dto::ScanVerdictDto>(
            openapi,
            StatusCode::OK,
            "Scan verdicts",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/prompt-scanner/v1/policy")
        .operation_id("prompt_scanner.get_policy")
        .summary("Get the calling tenant's backend policy")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::get_policy)
        .json_response_with_schema::<dto::TenantPolicyDto>(
            openapi,
            StatusCode::OK,
            "Tenant backend policy",
        )
        .standard_errors(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::put("/prompt-scanner/v1/policy")
        .operation_id("prompt_scanner.put_policy")
        .summary("Update the calling tenant's backend policy")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::TenantPolicyUpdateDto>(openapi, "New tenant backend policy")
        .handler(handlers::put_policy)
        .json_response_with_schema::<dto::TenantPolicyDto>(
            openapi,
            StatusCode::OK,
            "Updated tenant backend policy",
        )
        .standard_errors(openapi)
        .problem_response(
            openapi,
            StatusCode::NOT_IMPLEMENTED,
            "The active policy provider is read-only",
        )
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/prompt-scanner/v1/backends")
        .operation_id("prompt_scanner.list_backends")
        .summary("List registered scanner backend instances and their availability")
        .tag(API_TAG)
        .authenticated()
        .require_license_features::<License>([])
        .handler(handlers::list_backends)
        .json_array_response_with_schema::<dto::BackendInstanceDto>(
            openapi,
            StatusCode::OK,
            "Registered scanner backend instances",
        )
        .standard_errors(openapi)
        .register(router, openapi);

    router.layer(axum::Extension(service))
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod routes_tests;
