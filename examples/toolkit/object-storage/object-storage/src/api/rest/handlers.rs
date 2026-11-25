use std::sync::Arc;

use axum::Json;
use axum::extract::{Extension, Path};
use toolkit::api::canonical_prelude::*;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{CreateObjectRequest, ObjectDto, PatchObjectRequest};
use super::routes::ConcreteService;

pub async fn create_object(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Json(req): Json<CreateObjectRequest>,
) -> ApiResult<impl IntoResponse> {
    let object = svc.create_object(&ctx, req.into()).await?;
    Ok((StatusCode::CREATED, Json(ObjectDto::from(object))))
}

pub async fn get_object(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(id): Path<Uuid>,
) -> ApiResult<JsonBody<ObjectDto>> {
    let object = svc.get_object(&ctx, id).await?;
    Ok(Json(object.into()))
}

pub async fn update_object(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchObjectRequest>,
) -> ApiResult<JsonBody<ObjectDto>> {
    let object = svc.update_object(&ctx, id, req.into()).await?;
    Ok(Json(object.into()))
}

pub async fn delete_object(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<ConcreteService>>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    svc.delete_object(&ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
