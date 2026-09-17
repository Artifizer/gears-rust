//! Plugin API trait implemented by scanner backend plugins.
//!
//! # Fan-out, not pick-one
//!
//! Unlike most plugin extension points in this platform (which select a
//! *single* plugin instance by vendor+priority via `choose_plugin_instance`),
//! `prompt-scanner` fans out to **every** scanner backend plugin instance
//! enabled for the calling tenant's policy, concurrently, with a per-backend
//! timeout. See `docs/DESIGN.md`.
//!
//! Each scanner backend plugin registers:
//! 1. A `PluginV1<ScannerBackendSpecV1>` instance in `types-registry`
//!    (discoverable by the gateway).
//! 2. A scoped `ClientHub` entry for `dyn ScannerBackendClient`, keyed by
//!    the instance's GTS id (`ClientScope::gts_id`).

use async_trait::async_trait;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::errors::PromptScannerError;
use crate::models::{BackendOutcome, ScanInput, TenantPolicy};

/// Plugin API trait implemented by each scanner backend (DLP, secrets,
/// prompt injection, harmful content, malicious code/URL, ...).
///
/// # Security Context
///
/// The gateway passes the `SecurityContext` through without validation.
/// Backends that need to enforce their own access rules may do so, but
/// tenant isolation for *which* backends run is already enforced by the
/// gateway via `TenantPolicy.enabled_backends` before this trait is ever
/// invoked.
#[async_trait]
pub trait ScannerBackendClient: Send + Sync {
    /// Run this backend against the given input and return its detections.
    /// The gateway stamps each [`Detection`](crate::Detection) with this
    /// backend's identity and [`Self::version`].
    ///
    /// Implementations should return quickly — the gateway applies a
    /// per-backend timeout (the gear's `backend_timeout` setting) and
    /// treats a slow backend the same as one that errored: unless the tenant
    /// policy marks this backend `required`, the scan goes on without it and
    /// lists it in `ScanVerdict::unscored_backends`.
    ///
    /// # Deferral
    ///
    /// On an inline run ([`ScanInput::is_deferred_run`] is `false`), a
    /// backend whose input exceeds its own cost threshold (e.g. an oversized
    /// payload for an ONNX model) may return [`BackendOutcome::Defer`]
    /// rather than blocking the request. The gateway then calls `detect`
    /// once more in the background with the same input,
    /// [`ScanInput::phase`] set to
    /// [`ScanPhase::Deferred`](crate::ScanPhase::Deferred) and the longer
    /// `deferred_backend_timeout`, and appends the findings to the scan.
    ///
    /// On that deferred run the backend **must not** return `Defer`: it
    /// scans the input in full, or returns an error. A `Defer` there is
    /// treated as a backend failure (counted and logged; it fails an async
    /// job whose policy requires the backend). See `docs/DESIGN.md`'s "Async
    /// deferral for oversized inputs".
    ///
    /// # Errors
    ///
    /// Implementations may return any [`PromptScannerError`]; the gateway
    /// treats all of them as "this backend did not produce a finding" for
    /// aggregation, unless the tenant policy marks this backend `required`.
    /// [`PromptScannerError::BackendUnavailable`] (e.g. a model that is not
    /// loaded) is reported as `unavailable`, any other error as `error`.
    async fn detect(
        &self,
        ctx: &SecurityContext,
        input: &ScanInput,
    ) -> Result<BackendOutcome, PromptScannerError>;

    /// This backend's version, at most 64 characters. `prompt-scanner`
    /// stamps it on each finding made from the backend's detections
    /// (`Finding::detector_version`) and stores it with them, so a finding
    /// can be traced to the scanner build that produced it. `None` when the
    /// backend reports no version.
    fn version(&self) -> Option<String> {
        None
    }
}

/// Plugin API trait implemented by whatever currently owns tenant policy
/// storage/decisioning for `prompt-scanner`.
///
/// # Single-instance selection, not fan-out
///
/// Unlike [`ScannerBackendClient`], exactly one `PolicyProvider` is
/// authoritative at a time — there's no sound way to "aggregate" two
/// different tenant policies. `prompt-scanner` resolves the
/// highest-priority registered `PolicyProviderSpecV1` instance (see
/// `crate::gts::PolicyProviderSpecV1`), falling back to its own built-in
/// read-only provider, which serves one policy from the gear's config, when
/// no plugin instance is registered. prompt-scanner stores no tenant
/// policies itself.
///
/// # Transport-agnostic
///
/// Deliberately does not take a `SecureORM` runner/scope — a provider backed
/// by an external policy service has no database connection to share.
/// Implementations are responsible for their own authorization if they need
/// it. `Service` only ever asks for the calling subject's own tenant on
/// `GET`/`PUT /policy`, and for the scanned tenant after authorizing the
/// scan.
#[async_trait]
pub trait PolicyProvider: Send + Sync {
    /// Fetch the policy configured for `tenant_id`, if any. `None` means
    /// "nothing configured for this tenant" — the caller falls back to its
    /// default policy.
    ///
    /// # Errors
    ///
    /// Returns [`PromptScannerError`] on authorization failure, or
    /// [`PromptScannerError::PolicyUnavailable`] when the provider's source
    /// is unreachable and it has no policy to serve for the tenant.
    async fn get_policy(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Option<TenantPolicy>, PromptScannerError>;

    /// Insert or replace the policy for `policy.tenant_id`, returning the
    /// stored value.
    ///
    /// # Errors
    ///
    /// Returns [`PromptScannerError`] on authorization failure, validation
    /// failure, or backend unavailability, and
    /// [`PromptScannerError::Unsupported`] when the provider is read-only.
    async fn put_policy(
        &self,
        ctx: &SecurityContext,
        policy: TenantPolicy,
    ) -> Result<TenantPolicy, PromptScannerError>;
}
