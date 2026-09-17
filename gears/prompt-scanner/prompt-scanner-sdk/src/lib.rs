//! Scan Gateway SDK
//!
//! Public API for the `prompt-scanner` gear — a tenant-aware content-scanning
//! / verdict microservice. See `gears/prompt-scanner/docs/DESIGN.md` for the
//! full design.
//!
//! - [`PromptScannerClientV1`] — public API trait, consumed by connectors
//!   (endpoint agents, `llm-gateway`'s `scan-hook-plugin`, MCP proxies, ...).
//! - [`ScannerBackendClient`] — plugin API trait, implemented by scanner
//!   backends (DLP, secrets, prompt injection, harmful content, malicious
//!   code/URL, ...).
//! - [`ScannerBackendSpecV1`] — GTS schema scanner backend plugins register
//!   an instance of, for discovery via `types-registry`.
//! - Domain models: [`ScanRequest`], [`ScanVerdict`], [`Detection`], [`Finding`],
//!   [`VerdictAction`], [`TenantPolicy`], ...
//! - [`PromptScannerError`] — transport-agnostic error type.
//!
//! ## Usage
//!
//! ```ignore
//! use prompt_scanner_sdk::{ContentKind, PromptScannerClientV1, ScanRequest};
//!
//! let client = hub.get::<dyn PromptScannerClientV1>()?;
//! let verdict = client
//!     .scan(&ctx, ScanRequest::text("llm-gateway", ContentKind::Prompt, "..."))
//!     .await?;
//! ```

#![forbid(unsafe_code)]

pub mod api;
pub mod errors;
pub mod gts;
pub mod models;
pub mod plugin_api;
pub mod redaction;

// Re-export main types at crate root
pub use api::PromptScannerClientV1;
pub use errors::PromptScannerError;
pub use gts::{PolicyProviderSpecV1, ScannerBackendSpecV1};
pub use models::{
    Attachment, BackendOutcome, ContentKind, Detection, EvidencePolicy, Finding,
    InvalidScanRequest, MAX_ATTACHMENT_BYTES, MAX_CONNECTOR_ID_LEN, MAX_CORRELATION_ID_LEN,
    MAX_FILENAME_LEN, MAX_METADATA_ENTRIES, MAX_METADATA_KEY_LEN, MAX_METADATA_VALUE_LEN,
    MAX_MIME_LEN, MAX_PAYLOAD_BYTES, ScanContent, ScanInput, ScanJobFailureCode, ScanJobHandle,
    ScanJobStatus, ScanPhase, ScanRequest, ScanVerdict, Severity, Span, TenantPolicy,
    UnscoredBackend, UnscoredReason, VerdictAction, validate_correlation_id,
};
pub use plugin_api::{PolicyProvider, ScannerBackendClient};
pub use redaction::{REDACTED_PLACEHOLDER, redact_spans};
