//! Fact providers: how this gear fulfils the Facts of its Governed Operations.
//!
//! Each provider implements the trait `#[derive(GovernedFacts)]` generated for
//! its operation type, one typed method per Fact, so a missing, misspelled, or
//! mistyped Fact is a compile error. The method shape follows the tier:
//!
//! - **request / computed** — `fn fact(input: &Self::Input)`: synchronous, and
//!   given only the operation's data. The repository and connection the
//!   provider holds are not reachable from it.
//! - **costly** — `async fn fact(&self)`: may read the database.
//!
//! The Enforcer calls only the methods for Facts the applicable policies read.
//! That is what makes costly Facts on-demand: `owner_object_count` is a query
//! only when a policy reads it, and deleting an object reads the object only
//! when a policy reads one of its Facts.
//!
//! Wrap a provider in the generated `XProvider` to pass it to the Enforcer.

use object_storage_sdk::gts::{
    CreateObjectV1Facts, DeleteObjectV1Facts, GetObjectV1Facts, UpdateObjectV1Facts,
};
use object_storage_sdk::{NewObject, StoredObject};
use operation_governance_sdk::{FactResultExt, FactValue, LazySource};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::repo::ObjectsRepository;

/// Seconds since the gear created the object: a Fact built from generated data.
fn age_seconds(object: &StoredObject, now: OffsetDateTime) -> i64 {
    (now - object.created_at).whole_seconds().max(0)
}

fn size_of(body: &str) -> u64 {
    u64::try_from(body.len()).unwrap_or(u64::MAX)
}

fn non_negative(size: i64) -> u64 {
    u64::try_from(size).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// create
// ---------------------------------------------------------------------------

/// Facts of creating an object. Request and computed Facts come from the
/// [`NewObject`]; `owner_object_count` is counted on demand.
pub struct CreateOpFacts<'a, R, C> {
    pub repo: &'a R,
    pub conn: &'a C,
    pub scope: &'a AccessScope,
    pub new: &'a NewObject,
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
}

#[async_trait::async_trait]
impl<R, C> CreateObjectV1Facts for CreateOpFacts<'_, R, C>
where
    R: ObjectsRepository,
    C: DBRunner + Sync,
{
    type Input = NewObject;

    fn input(&self) -> &NewObject {
        self.new
    }

    fn name(input: &NewObject) -> FactValue<String> {
        Ok(input.name.clone())
    }

    fn content_type(input: &NewObject) -> FactValue<String> {
        Ok(input.content_type.clone())
    }

    fn size_bytes(input: &NewObject) -> FactValue<u64> {
        Ok(size_of(&input.body))
    }

    fn label_count(input: &NewObject) -> FactValue<u32> {
        Ok(u32::try_from(input.labels.len()).unwrap_or(u32::MAX))
    }

    async fn owner_object_count(&self) -> FactValue<u64> {
        self.repo
            .count_by_owner(self.conn, self.scope, self.tenant_id, self.owner_id)
            .await
            .or_unavailable()
    }
}

// ---------------------------------------------------------------------------
// get / update: the service has the object in memory, so every Fact is computed
// ---------------------------------------------------------------------------

/// An object the service already read, seen by one caller at one instant.
pub struct ObjectView<'a> {
    pub object: &'a StoredObject,
    pub caller: Uuid,
    pub now: OffsetDateTime,
}

/// Facts of reading an object.
pub struct GetOpFacts<'a> {
    pub view: ObjectView<'a>,
}

#[async_trait::async_trait]
impl<'a> GetObjectV1Facts for GetOpFacts<'a> {
    type Input = ObjectView<'a>;

    fn input(&self) -> &ObjectView<'a> {
        &self.view
    }

    fn content_type(v: &ObjectView<'a>) -> FactValue<String> {
        Ok(v.object.content_type.clone())
    }

    fn labels(v: &ObjectView<'a>) -> FactValue<std::collections::BTreeMap<String, String>> {
        Ok(v.object.labels.clone())
    }

    fn age_seconds(v: &ObjectView<'a>) -> FactValue<i64> {
        Ok(age_seconds(v.object, v.now))
    }

    fn is_owner(v: &ObjectView<'a>) -> FactValue<bool> {
        Ok(v.object.owner_id == v.caller)
    }
}

/// An object before and after a change.
pub struct ChangeView<'a> {
    pub before: &'a StoredObject,
    pub after: &'a StoredObject,
    pub caller: Uuid,
    pub now: OffsetDateTime,
}

/// Facts of changing an object.
pub struct UpdateOpFacts<'a> {
    pub change: ChangeView<'a>,
}

#[async_trait::async_trait]
impl<'a> UpdateObjectV1Facts for UpdateOpFacts<'a> {
    type Input = ChangeView<'a>;

    fn input(&self) -> &ChangeView<'a> {
        &self.change
    }

    fn content_type(c: &ChangeView<'a>) -> FactValue<String> {
        Ok(c.after.content_type.clone())
    }

    fn size_bytes(c: &ChangeView<'a>) -> FactValue<u64> {
        Ok(non_negative(c.after.size_bytes))
    }

    fn size_delta_bytes(c: &ChangeView<'a>) -> FactValue<i64> {
        Ok(c.after.size_bytes - c.before.size_bytes)
    }

    fn age_seconds(c: &ChangeView<'a>) -> FactValue<i64> {
        Ok(age_seconds(c.before, c.now))
    }

    fn is_owner(c: &ChangeView<'a>) -> FactValue<bool> {
        Ok(c.before.owner_id == c.caller)
    }
}

// ---------------------------------------------------------------------------
// delete: every Fact is costly; the object is read once, only if needed
// ---------------------------------------------------------------------------

/// Facts of deleting an object. The service deletes by ID without reading the
/// object; the first costly Fact a policy reads loads it, the rest reuse it.
pub struct DeleteOpFacts<'a, R, C> {
    pub repo: &'a R,
    pub conn: &'a C,
    pub scope: &'a AccessScope,
    pub id: Uuid,
    pub caller: Uuid,
    pub now: OffsetDateTime,
    object: LazySource<StoredObject>,
}

impl<'a, R, C> DeleteOpFacts<'a, R, C>
where
    R: ObjectsRepository,
    C: DBRunner + Sync,
{
    pub fn new(
        repo: &'a R,
        conn: &'a C,
        scope: &'a AccessScope,
        id: Uuid,
        caller: Uuid,
        now: OffsetDateTime,
    ) -> Self {
        Self {
            repo,
            conn,
            scope,
            id,
            caller,
            now,
            object: LazySource::new(),
        }
    }

    /// Whether a policy needed the object and it does not exist.
    pub fn object_missing(&self) -> bool {
        self.object.is_missing()
    }

    async fn object(&self) -> FactValue<&StoredObject> {
        self.object
            .get(|| self.repo.find(self.conn, self.scope, self.id))
            .await
    }
}

#[async_trait::async_trait]
impl<R, C> DeleteObjectV1Facts for DeleteOpFacts<'_, R, C>
where
    R: ObjectsRepository,
    C: DBRunner + Sync,
{
    async fn content_type(&self) -> FactValue<String> {
        Ok(self.object().await?.content_type.clone())
    }

    async fn size_bytes(&self) -> FactValue<u64> {
        Ok(non_negative(self.object().await?.size_bytes))
    }

    async fn age_seconds(&self) -> FactValue<i64> {
        Ok(age_seconds(self.object().await?, self.now))
    }

    async fn is_owner(&self) -> FactValue<bool> {
        Ok(self.object().await?.owner_id == self.caller)
    }
}
