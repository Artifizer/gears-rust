//! Domain service: Authorization → Governance Check → write.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::{AccessRequest, ResourceType};
use object_storage_sdk::gts::{
    CreateObjectV1Provider, DeleteObjectV1Provider, GetObjectV1Provider, UpdateObjectV1Provider,
};
use object_storage_sdk::{NewObject, OBJECT_RESOURCE_TYPE, ObjectPatch, StoredObject};
use operation_governance_sdk::{
    CheckMode, FactProvider, GovernanceEnforcer, GovernanceOutcome, GovernedResource, PolicyFinding,
};
use time::OffsetDateTime;
use toolkit_db::DBProvider;
use toolkit_macros::domain_model;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

use super::error::DomainError;
use super::governance::{
    ChangeView, CreateOpFacts, DeleteOpFacts, GetOpFacts, ObjectView, UpdateOpFacts,
};
use super::repo::ObjectsRepository;

pub(crate) type DbProvider = DBProvider<toolkit_db::DbError>;

/// Authorization resource type of a stored object.
pub(crate) const OBJECT_RESOURCE: ResourceType = ResourceType::from_static(
    OBJECT_RESOURCE_TYPE,
    &[pep_properties::OWNER_TENANT_ID, pep_properties::RESOURCE_ID],
);

pub(crate) mod actions {
    pub const CREATE: &str = "create";
    pub const GET: &str = "get";
    pub const UPDATE: &str = "update";
    pub const DELETE: &str = "delete";
}

#[domain_model]
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub max_name_len: usize,
    pub max_body_bytes: usize,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            max_name_len: 255,
            max_body_bytes: 1024 * 1024,
        }
    }
}

#[domain_model]
pub struct Service<R: ObjectsRepository> {
    db: Arc<DbProvider>,
    repo: Arc<R>,
    authz: PolicyEnforcer,
    governance: Arc<GovernanceEnforcer>,
    config: ServiceConfig,
}

impl<R: ObjectsRepository> Service<R> {
    pub fn new(
        db: Arc<DbProvider>,
        repo: Arc<R>,
        authz: PolicyEnforcer,
        governance: Arc<GovernanceEnforcer>,
        config: ServiceConfig,
    ) -> Self {
        Self {
            db,
            repo,
            authz,
            governance,
            config,
        }
    }

    #[must_use]
    pub fn max_body_bytes(&self) -> usize {
        self.config.max_body_bytes
    }

    /// Create an object: Authorization, then governance, then the insert.
    ///
    /// # Errors
    /// [`DomainError`] when the input is invalid, access is denied, the object
    /// is missing, governance refuses or cannot decide, or storage fails.
    pub async fn create_object(
        &self,
        ctx: &SecurityContext,
        new: NewObject,
    ) -> Result<StoredObject, DomainError> {
        self.validate(Some(&new.name), Some(&new.content_type), Some(&new.body))?;
        let tenant_id = ctx.subject_tenant_id();

        // 1. Authorization.
        let scope = self
            .authorize(ctx, actions::CREATE, None, tenant_id)
            .await?;
        let conn = self.db.conn()?;

        // 2. Governance, before the first side effect. `owner_object_count` is
        //    counted only if a policy reads it.
        let facts = CreateObjectV1Provider(CreateOpFacts {
            repo: self.repo.as_ref(),
            conn: &conn,
            scope: &scope,
            new: &new,
            tenant_id,
            owner_id: ctx.subject_id(),
        });
        self.govern(
            ctx,
            GovernedResource::new(OBJECT_RESOURCE_TYPE, tenant_id),
            &facts,
        )
        .await?;

        // 3. The write. `created_at` and `updated_at` are generated here, and
        //    later operations can govern on them (`age_seconds`).
        let now = OffsetDateTime::now_utc();
        let object = StoredObject {
            id: Uuid::now_v7(),
            tenant_id,
            owner_id: ctx.subject_id(),
            size_bytes: i64::try_from(new.body.len()).unwrap_or(i64::MAX),
            name: new.name,
            content_type: new.content_type,
            labels: new.labels,
            body: new.body,
            created_at: now,
            updated_at: now,
        };
        self.repo.insert(&conn, &scope, object).await
    }

    /// Read an object; governed on Facts computed from the stored object.
    ///
    /// # Errors
    /// [`DomainError`] when the input is invalid, access is denied, the object
    /// is missing, governance refuses or cannot decide, or storage fails.
    pub async fn get_object(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<StoredObject, DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let scope = self
            .authorize(ctx, actions::GET, Some(id), tenant_id)
            .await?;
        let conn = self.db.conn()?;
        let object = self
            .repo
            .find(&conn, &scope, id)
            .await?
            .ok_or(DomainError::NotFound(id))?;

        let facts = GetObjectV1Provider(GetOpFacts {
            view: ObjectView {
                object: &object,
                caller: ctx.subject_id(),
                now: OffsetDateTime::now_utc(),
            },
        });
        self.govern(ctx, resource_of(&object), &facts).await?;
        Ok(object)
    }

    /// Change an object; governed on the object before and after the change.
    ///
    /// # Errors
    /// [`DomainError`] when the input is invalid, access is denied, the object
    /// is missing, governance refuses or cannot decide, or storage fails.
    pub async fn update_object(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: ObjectPatch,
    ) -> Result<StoredObject, DomainError> {
        self.validate(
            patch.name.as_ref(),
            patch.content_type.as_ref(),
            patch.body.as_ref(),
        )?;
        let tenant_id = ctx.subject_tenant_id();
        let scope = self
            .authorize(ctx, actions::UPDATE, Some(id), tenant_id)
            .await?;
        let conn = self.db.conn()?;
        let before = self
            .repo
            .find(&conn, &scope, id)
            .await?
            .ok_or(DomainError::NotFound(id))?;
        let now = OffsetDateTime::now_utc();
        let after = apply(&before, patch, now);

        let facts = UpdateObjectV1Provider(UpdateOpFacts {
            change: ChangeView {
                before: &before,
                after: &after,
                caller: ctx.subject_id(),
                now,
            },
        });
        self.govern(ctx, resource_of(&before), &facts).await?;

        self.repo.update(&conn, &scope, after).await
    }

    /// Delete an object; its Facts are loaded only if a policy reads them.
    ///
    /// # Errors
    /// [`DomainError`] when the input is invalid, access is denied, the object
    /// is missing, governance refuses or cannot decide, or storage fails.
    pub async fn delete_object(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), DomainError> {
        let tenant_id = ctx.subject_tenant_id();
        let scope = self
            .authorize(ctx, actions::DELETE, Some(id), tenant_id)
            .await?;
        let conn = self.db.conn()?;

        let facts = DeleteObjectV1Provider(DeleteOpFacts::new(
            self.repo.as_ref(),
            &conn,
            &scope,
            id,
            ctx.subject_id(),
            OffsetDateTime::now_utc(),
        ));
        let resource =
            GovernedResource::new(OBJECT_RESOURCE_TYPE, tenant_id).with_id(id.to_string());
        let outcome = self.check(ctx, resource, &facts).await;
        // The provider read the object only if a policy needed it; if it found
        // nothing, the answer is "not found", not a governance refusal.
        if facts.0.object_missing() {
            return Err(DomainError::NotFound(id));
        }
        refuse_unless_allowed(outcome)?;

        if self.repo.delete(&conn, &scope, id).await? {
            Ok(())
        } else {
            Err(DomainError::NotFound(id))
        }
    }

    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
        id: Option<Uuid>,
        tenant_id: Uuid,
    ) -> Result<AccessScope, DomainError> {
        Ok(self
            .authz
            .access_scope_with(
                ctx,
                &OBJECT_RESOURCE,
                action,
                id,
                &AccessRequest::new().resource_property(pep_properties::OWNER_TENANT_ID, tenant_id),
            )
            .await?)
    }

    async fn check<P: FactProvider>(
        &self,
        ctx: &SecurityContext,
        resource: GovernedResource,
        facts: &P,
    ) -> GovernanceOutcome {
        self.governance
            .check(ctx, resource, CheckMode::Enforce, facts)
            .await
    }

    async fn govern<P: FactProvider>(
        &self,
        ctx: &SecurityContext,
        resource: GovernedResource,
        facts: &P,
    ) -> Result<(), DomainError> {
        refuse_unless_allowed(self.check(ctx, resource, facts).await)
    }

    fn validate(
        &self,
        name: Option<&String>,
        content_type: Option<&String>,
        body: Option<&String>,
    ) -> Result<(), DomainError> {
        if let Some(name) = name
            && (name.trim().is_empty() || name.len() > self.config.max_name_len)
        {
            return Err(DomainError::validation(
                "name",
                format!("must be 1-{} characters", self.config.max_name_len),
            ));
        }
        if let Some(content_type) = content_type
            && !content_type.contains('/')
        {
            return Err(DomainError::validation(
                "content_type",
                "must be a MIME type such as text/plain",
            ));
        }
        if let Some(body) = body
            && body.len() > self.config.max_body_bytes
        {
            return Err(DomainError::validation(
                "body",
                format!("must be at most {} bytes", self.config.max_body_bytes),
            ));
        }
        Ok(())
    }
}

fn resource_of(object: &StoredObject) -> GovernedResource {
    GovernedResource::new(OBJECT_RESOURCE_TYPE, object.tenant_id)
        .with_id(object.id.to_string())
        .with_version(object.updated_at.unix_timestamp_nanos().to_string())
}

fn apply(before: &StoredObject, patch: ObjectPatch, now: OffsetDateTime) -> StoredObject {
    let mut after = before.clone();
    if let Some(name) = patch.name {
        after.name = name;
    }
    if let Some(content_type) = patch.content_type {
        after.content_type = content_type;
    }
    if let Some(labels) = patch.labels {
        after.labels = labels;
    }
    if let Some(body) = patch.body {
        after.size_bytes = i64::try_from(body.len()).unwrap_or(i64::MAX);
        after.body = body;
    }
    after.updated_at = now;
    after
}

fn joined(findings: &[PolicyFinding]) -> String {
    findings
        .iter()
        .map(|f| format!("{}: {}", f.policy, f.message))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Map a Governance Outcome to the gear's errors. Only Allowed proceeds.
fn refuse_unless_allowed(outcome: GovernanceOutcome) -> Result<(), DomainError> {
    match outcome {
        GovernanceOutcome::Allowed => Ok(()),
        GovernanceOutcome::Blocked { findings } => {
            Err(DomainError::GovernanceBlocked(joined(&findings)))
        }
        GovernanceOutcome::ApprovalRequired { findings } => {
            Err(DomainError::ApprovalRequired(joined(&findings)))
        }
        // An undeclared or malformed Fact is a defect in this gear, not the
        // caller's input.
        GovernanceOutcome::Invalid { message } => Err(DomainError::Internal(format!(
            "governance rejected the gear's facts: {message}"
        ))),
        GovernanceOutcome::Unavailable { message, retryable } => {
            Err(DomainError::GovernanceUnavailable { message, retryable })
        }
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;
