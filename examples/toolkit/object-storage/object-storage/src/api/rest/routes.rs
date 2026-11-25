use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::{Extension, Router};
use toolkit::api::operation_builder::{CORE_GLOBAL_BASE_LICENSE_FEATURE, LicenseFeature};
use toolkit::api::{OpenApiRegistry, OperationBuilder};

use super::{dto, handlers};
use crate::domain::service::Service;
use crate::infra::storage::sea_orm_repo::SeaOrmObjectsRepository;

pub type ConcreteService = Service<SeaOrmObjectsRepository>;

struct License;

impl AsRef<str> for License {
    fn as_ref(&self) -> &'static str {
        CORE_GLOBAL_BASE_LICENSE_FEATURE
    }
}

impl LicenseFeature for License {}

const ID_DOC: &str = "Object ID (UUID)";

pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<ConcreteService>,
) -> Router {
    // Governance refusals surface as 403 (blocked), 412 (approval required),
    // and 503 (governance could not decide), next to the usual errors.
    router = OperationBuilder::post("/object-storage/v1/objects")
        .operation_id("object_storage.create_object")
        .summary("Create an object")
        .tag("Objects")
        .authenticated()
        .require_license_features::<License>([])
        .json_request::<dto::CreateObjectRequest>(openapi, "New object")
        .handler(handlers::create_object)
        .json_response_with_schema::<dto::ObjectDto>(openapi, StatusCode::CREATED, "Object created")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_413(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/object-storage/v1/objects/{id}")
        .operation_id("object_storage.get_object")
        .summary("Get an object")
        .tag("Objects")
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", ID_DOC)
        .handler(handlers::get_object)
        .json_response_with_schema::<dto::ObjectDto>(openapi, StatusCode::OK, "Object")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::patch("/object-storage/v1/objects/{id}")
        .operation_id("object_storage.update_object")
        .summary("Change an object")
        .tag("Objects")
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", ID_DOC)
        .json_request::<dto::PatchObjectRequest>(openapi, "Fields to change")
        .handler(handlers::update_object)
        .json_response_with_schema::<dto::ObjectDto>(openapi, StatusCode::OK, "Object changed")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_413(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    router = OperationBuilder::delete("/object-storage/v1/objects/{id}")
        .operation_id("object_storage.delete_object")
        .summary("Delete an object")
        .tag("Objects")
        .authenticated()
        .require_license_features::<License>([])
        .path_param("id", ID_DOC)
        .handler(handlers::delete_object)
        .no_content_response(StatusCode::NO_CONTENT, "Object deleted")
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);

    // Room for a body at its bound inside the JSON envelope.
    let body_limit = service
        .max_body_bytes()
        .saturating_mul(2)
        .saturating_add(4096);
    router
        .layer(DefaultBodyLimit::max(body_limit))
        .layer(Extension(service))
}
