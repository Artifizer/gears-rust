//! Object Storage SDK (example)
//!
//! The public contract of the `object-storage` example gear:
//!
//! - [`ObjectStorageClientV1`] — create, get, update, and delete stored objects.
//! - [`models`] — [`StoredObject`], [`NewObject`], [`ObjectPatch`].
//! - [`gts`] — the gear's **Governed Operation types**, one per operation. They
//!   are the governance contract that policy authors write against, and they
//!   are deliberately separate from the REST DTOs: renaming a route or changing
//!   a DTO never changes a Fact a policy reads.
#![forbid(unsafe_code)]

pub mod api;
pub mod gts;
pub mod models;

pub use api::ObjectStorageClientV1;
pub use gts::{CreateObjectV1, DeleteObjectV1, GetObjectV1, OBJECT_RESOURCE_TYPE, UpdateObjectV1};
pub use models::{NewObject, ObjectPatch, StoredObject};
