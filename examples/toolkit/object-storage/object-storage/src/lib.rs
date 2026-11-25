//! Object Storage (example gear)
//!
//! A small object store with a REST API and a database, written to show how a
//! gear puts its operations under **Operation Governance**. Read it in this
//! order:
//!
//! 1. `object_storage_sdk::gts` — one Governed Operation type per operation.
//!    Its fields are the Facts policies read; it is separate from the DTOs.
//! 2. [`domain::governance`] — one `FactProvider` per operation. Each fills
//!    only the Facts the Enforcer asks for: request Facts from the input,
//!    computed Facts in memory, and costly Facts (an owner's object count, an
//!    object's age built from its generated `created_at`) from the database,
//!    only when a policy reads them.
//! 3. [`domain::service`] — every operation runs Authorization, then the
//!    Governance Check, then the write. Only `Allowed` lets it proceed.
//! 4. [`gear`] — builds the `GovernanceEnforcer`, which finds the governance
//!    plugin through types-registry.
#![forbid(unsafe_code)]

pub mod gear;
pub use gear::ObjectStorageGear;

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod config;
pub mod domain;
#[doc(hidden)]
pub mod infra;
