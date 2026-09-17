//! Transport-agnostic domain models for the prompt-scanner gear.

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Kind of content submitted for scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    /// An outbound prompt sent to an LLM.
    Prompt,
    /// An inbound response returned by an LLM.
    Response,
    /// A file/blob upload.
    Upload,
    /// An MCP tool call (request side).
    McpToolCall,
    /// An MCP tool result (response side).
    McpToolResult,
}

impl ContentKind {
    /// Every content kind.
    pub const ALL: [Self; 5] = [
        Self::Prompt,
        Self::Response,
        Self::Upload,
        Self::McpToolCall,
        Self::McpToolResult,
    ];

    /// [`Self::ALL`] as a `Vec`: the default of `TenantPolicy::content_kinds`.
    #[must_use]
    pub fn all() -> Vec<Self> {
        Self::ALL.to_vec()
    }
}

/// A byte offset span within the scanned payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// Start offset (inclusive).
    pub start: u32,
    /// End offset (exclusive).
    pub end: u32,
}

/// Detector-reported severity for a finding.
///
/// Declaration order is significant: `derive(Ord)` ranks variants by
/// declaration order, so `Critical > High > Medium > Low`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational / low risk.
    Low,
    /// Moderate risk.
    Medium,
    /// Significant risk.
    High,
    /// Severe risk, should almost always block.
    Critical,
}

/// The gateway's verdict action for a scanned payload.
///
/// # Precedence
///
/// Declaration order is significant: `derive(Ord)` ranks variants by
/// declaration order, so the **verdict-aggregation precedence rule** used by
/// `Service::aggregate_verdict` is simply "take the maximum action across all
/// backend findings":
///
/// `Block > Redact > Warn > Audit > Allow`
///
/// This is a deliberately simple, documented policy (see
/// `gears/prompt-scanner/docs/DESIGN.md`, `cpt-cf-prompt-scanner-fr-verdict-aggregation`)
/// — a backend voting `Block` always wins over one voting
/// `Warn`, regardless of confidence/severity. More nuanced weighting
/// (confidence-adjusted precedence, per-tenant override of the ordering) is
/// left as future work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictAction {
    /// No issue found; allow the content through unchanged.
    #[default]
    Allow,
    /// Allow, but record the finding for audit trail purposes.
    Audit,
    /// Allow, but surface a warning to the caller.
    Warn,
    /// Allow, but redact the offending content first.
    Redact,
    /// Block the content entirely.
    Block,
}

/// One match reported by a scanner backend, before `prompt-scanner` stamps
/// the backend's identity on it.
///
/// Backends return these from
/// [`ScannerBackendClient::detect`](crate::ScannerBackendClient::detect);
/// the gateway turns each into a [`Finding`] (see
/// [`Finding::from_detection`]), filling `detector` and `detector_version`
/// itself so a backend can never claim to be another one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Detection {
    /// Backend-specific category (e.g. `"pii.email"`, `"secret.aws_key"`).
    pub category: String,
    /// Backend-reported severity.
    pub severity: Severity,
    /// Backend-reported confidence, in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Optional byte span within the scanned payload.
    pub span: Option<Span>,
    /// The action this backend suggests for this detection alone.
    pub suggested_action: VerdictAction,
}

impl Detection {
    /// A detection with no span. Add one with [`Self::with_span`].
    pub fn new(
        category: impl Into<String>,
        severity: Severity,
        confidence: f32,
        suggested_action: VerdictAction,
    ) -> Self {
        Self {
            category: category.into(),
            severity,
            confidence,
            span: None,
            suggested_action,
        }
    }

    /// Set the byte span within the scanned payload.
    #[must_use]
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }
}

/// A finding recorded in an aggregated [`ScanVerdict`]: a backend's
/// [`Detection`] stamped by `prompt-scanner` with the backend's identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// GTS instance id of the backend that produced this finding.
    pub detector: String,
    /// Backend-specific category (e.g. `"pii.email"`, `"secret.aws_key"`).
    pub category: String,
    /// Backend-reported severity.
    pub severity: Severity,
    /// Backend-reported confidence, in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Optional span within the payload where the finding was detected.
    pub span: Option<Span>,
    /// The action this backend suggests for this finding alone.
    pub suggested_action: VerdictAction,
    /// Version of the backend that produced this finding, from
    /// [`ScannerBackendClient::version`](crate::ScannerBackendClient::version).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detector_version: Option<String>,
}

impl Finding {
    /// Stamp a backend's `detection` with the backend's identity.
    #[must_use]
    pub fn from_detection(
        detection: Detection,
        detector: impl Into<String>,
        detector_version: Option<String>,
    ) -> Self {
        let Detection {
            category,
            severity,
            confidence,
            span,
            suggested_action,
        } = detection;
        Self {
            detector: detector.into(),
            category,
            severity,
            confidence,
            span,
            suggested_action,
            detector_version,
        }
    }
}

/// A backend's outcome for a single `detect` call: zero or more detections,
/// or `Defer`.
///
/// `Detections` carries *every* match a backend found in one scan (a message
/// can legitimately contain more than one distinct piece of PII/secret
/// material) — not just the single highest-priority one. An empty vec means
/// "no issue found".
///
/// `Defer` is distinct from an error: a backend that errors is treated as
/// "did not produce a finding" unless the tenant policy marks it required,
/// but a backend that defers has its work pushed onto the gateway's job
/// queue instead of being dropped. The gateway then calls the backend once
/// more with [`ScanInput::phase`] set to [`ScanPhase::Deferred`]; on that
/// run the backend must return `Detections` (or an error), never `Defer`
/// again — a second `Defer` counts as a backend failure. See
/// `gears/prompt-scanner/docs/DESIGN.md`'s "Async deferral for oversized
/// inputs".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BackendOutcome {
    /// Every detection this backend produced for the scanned payload — may
    /// be empty.
    Detections(Vec<Detection>),
    /// The backend is deferring this input — too expensive to evaluate
    /// synchronously (e.g. the payload exceeds the backend's own configured
    /// size threshold). The orchestrator queues this backend's contribution
    /// for background processing under the scan's existing `trace_id`.
    /// Only valid on a [`ScanPhase::Inline`] run.
    Defer,
}

/// Which run of a backend a `detect` call is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ScanPhase {
    /// The first call, on the scan's request path. The backend may return
    /// [`BackendOutcome::Defer`] for input it finds too expensive.
    #[default]
    Inline,
    /// The background retry of a backend that returned
    /// [`BackendOutcome::Defer`] inline, with the gateway's (longer)
    /// `deferred_backend_timeout`. The backend must scan the input now; a
    /// `Defer` here is treated as a backend failure.
    Deferred,
}

/// Longest accepted text payload ([`ScanContent::Text`]), in bytes (4 MiB).
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

/// Largest accepted raw attachment ([`ScanContent::File`]), in bytes
/// (25 MiB).
pub const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;

/// Most entries accepted in `ScanRequest::metadata`.
pub const MAX_METADATA_ENTRIES: usize = 32;

/// Longest accepted metadata key, in bytes.
pub const MAX_METADATA_KEY_LEN: usize = 128;

/// Longest accepted metadata value, in bytes.
pub const MAX_METADATA_VALUE_LEN: usize = 1024;

/// Longest accepted `Attachment::filename`, in characters.
pub const MAX_FILENAME_LEN: usize = 255;

/// Longest accepted `Attachment::mime_type`, in characters.
pub const MAX_MIME_LEN: usize = 127;

/// Longest accepted `ScanRequest::connector_id`, in characters.
pub const MAX_CONNECTOR_ID_LEN: usize = 255;

/// Longest accepted `ScanRequest::correlation_id`, in characters.
pub const MAX_CORRELATION_ID_LEN: usize = 255;

/// Prints a secret-bearing value as its length only.
struct Redacted(usize);

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<redacted {} bytes>", self.0)
    }
}

/// Metadata printed as keys with value lengths.
struct RedactedMetadata<'a>(&'a HashMap<String, String>);

impl fmt::Debug for RedactedMetadata<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|(k, v)| (k, Redacted(v.len()))))
            .finish()
    }
}

/// Input passed to a scanner backend's `detect` call.
///
/// A narrowed view of [`ScanRequest`]: always plain text (an attachment has
/// already been turned into text by `prompt-scanner`), and no connector id.
/// `Debug` prints the payload and metadata values as lengths only.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ScanInput {
    /// Tenant the content belongs to.
    pub tenant_id: Uuid,
    /// Kind of content being scanned.
    pub content_kind: ContentKind,
    /// The text to scan.
    pub payload: String,
    /// Free-form metadata (e.g. model name, source IP).
    pub metadata: HashMap<String, String>,
    /// Which run this is: [`ScanPhase::Inline`] on the request path,
    /// [`ScanPhase::Deferred`] on the background retry of a backend that
    /// deferred. See [`Self::is_deferred_run`].
    #[serde(default)]
    pub phase: ScanPhase,
}

impl ScanInput {
    /// Inline-phase input with no metadata. Add some with
    /// [`Self::with_metadata`].
    pub fn new(tenant_id: Uuid, content_kind: ContentKind, payload: impl Into<String>) -> Self {
        Self {
            tenant_id,
            content_kind,
            payload: payload.into(),
            metadata: HashMap::new(),
            phase: ScanPhase::Inline,
        }
    }

    /// Set the phase. The gateway sets [`ScanPhase::Deferred`] on the
    /// background retry; backends only read it.
    #[must_use]
    pub fn with_phase(mut self, phase: ScanPhase) -> Self {
        self.phase = phase;
        self
    }

    /// `true` on the background retry of a backend that returned
    /// [`BackendOutcome::Defer`]. The backend must scan the input in full on
    /// this run: returning `Defer` again is a backend failure.
    #[must_use]
    pub fn is_deferred_run(&self) -> bool {
        self.phase == ScanPhase::Deferred
    }

    /// Set the free-form metadata.
    #[must_use]
    pub fn with_metadata(mut self, metadata: HashMap<String, String>) -> Self {
        self.metadata = metadata;
        self
    }
}

impl fmt::Debug for ScanInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScanInput")
            .field("tenant_id", &self.tenant_id)
            .field("content_kind", &self.content_kind)
            .field("payload", &Redacted(self.payload.len()))
            .field("metadata", &RedactedMetadata(&self.metadata))
            .field("phase", &self.phase)
            .finish()
    }
}

/// A raw file submitted for scanning as [`ScanContent::File`].
///
/// `prompt-scanner` extracts text from it via `file-parser` before running
/// the backend fan-out, so a connector sends raw bytes once rather than
/// calling `file-parser` itself. `Debug` prints the bytes as a length only.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Attachment {
    /// Original filename, if known — helps `file-parser` pick a parser when
    /// `mime_type` is absent or generic. At most [`MAX_FILENAME_LEN`]
    /// characters.
    pub filename: Option<String>,
    /// MIME type, if known. An untrusted hint. At most [`MAX_MIME_LEN`]
    /// characters.
    pub mime_type: Option<String>,
    /// Raw file bytes. At most [`MAX_ATTACHMENT_BYTES`].
    pub data: Bytes,
}

impl Attachment {
    /// An attachment with no filename or MIME type.
    pub fn new(data: impl Into<Bytes>) -> Self {
        Self {
            filename: None,
            mime_type: None,
            data: data.into(),
        }
    }

    /// Set the original filename.
    #[must_use]
    pub fn with_filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    /// Set the MIME type.
    #[must_use]
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }
}

impl fmt::Debug for Attachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Attachment")
            .field(
                "filename",
                &self.filename.as_ref().map(|n| Redacted(n.len())),
            )
            .field("mime_type", &self.mime_type)
            .field("data", &Redacted(self.data.len()))
            .finish()
    }
}

/// What a [`ScanRequest`] asks to scan: text, or a file to extract text from.
#[derive(Clone, PartialEq, Eq)]
pub enum ScanContent {
    /// Plain text. Allowed for every [`ContentKind`]. Non-empty, at most
    /// [`MAX_PAYLOAD_BYTES`].
    Text(String),
    /// A raw file. Only allowed with [`ContentKind::Upload`].
    File(Attachment),
}

impl ScanContent {
    /// The text, when this is [`ScanContent::Text`].
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::File(_) => None,
        }
    }
}

impl fmt::Debug for ScanContent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => f.debug_tuple("Text").field(&Redacted(text.len())).finish(),
            Self::File(attachment) => f.debug_tuple("File").field(attachment).finish(),
        }
    }
}

/// A [`ScanRequest`] field that failed [`ScanRequest::validate`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
#[non_exhaustive]
pub struct InvalidScanRequest {
    /// The offending field (`"payload"`, `"attachment"`, `"metadata"`, ...).
    pub field: &'static str,
    /// What is wrong with it. Never contains the field's value.
    pub message: String,
}

impl InvalidScanRequest {
    fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl From<InvalidScanRequest> for crate::PromptScannerError {
    fn from(e: InvalidScanRequest) -> Self {
        Self::validation(e.to_string())
    }
}

/// A request to scan a piece of content.
///
/// `Debug` prints the content and metadata values as lengths only.
#[derive(Clone, PartialEq)]
pub struct ScanRequest {
    /// Identifier of the connector submitting the request (e.g.
    /// `"llm-gateway"`, `"endpoint-agent"`). At most
    /// [`MAX_CONNECTOR_ID_LEN`] characters.
    pub connector_id: String,
    /// The consumer's own identifier for this request, an opaque string of
    /// at most [`MAX_CORRELATION_ID_LEN`] characters. Stored with the scan's
    /// findings so a scan can be found again by it.
    pub correlation_id: Option<String>,
    /// Kind of content being scanned.
    pub content_kind: ContentKind,
    /// What to scan.
    pub content: ScanContent,
    /// Free-form metadata (e.g. model name, source IP). At most
    /// [`MAX_METADATA_ENTRIES`] entries, keys at most
    /// [`MAX_METADATA_KEY_LEN`] bytes, values at most
    /// [`MAX_METADATA_VALUE_LEN`] bytes.
    pub metadata: HashMap<String, String>,
}

impl ScanRequest {
    /// A text scan request with no correlation id or metadata.
    pub fn text(
        connector_id: impl Into<String>,
        content_kind: ContentKind,
        text: impl Into<String>,
    ) -> Self {
        Self {
            connector_id: connector_id.into(),
            correlation_id: None,
            content_kind,
            content: ScanContent::Text(text.into()),
            metadata: HashMap::new(),
        }
    }

    /// A [`ContentKind::Upload`] request for a raw file, with no
    /// correlation id or metadata.
    pub fn file(connector_id: impl Into<String>, attachment: Attachment) -> Self {
        Self {
            connector_id: connector_id.into(),
            correlation_id: None,
            content_kind: ContentKind::Upload,
            content: ScanContent::File(attachment),
            metadata: HashMap::new(),
        }
    }

    /// Check the request against the documented limits.
    ///
    /// # Errors
    ///
    /// [`InvalidScanRequest`] naming the first offending field: an empty or
    /// too-long `connector_id`/`correlation_id`, empty or oversized content,
    /// a [`ScanContent::File`] with a kind other than
    /// [`ContentKind::Upload`], an over-long filename/MIME type, or too much
    /// metadata.
    pub fn validate(&self) -> Result<(), InvalidScanRequest> {
        if self.connector_id.trim().is_empty() {
            return Err(InvalidScanRequest::new(
                "connector_id",
                "connector_id must not be empty",
            ));
        }
        if self.connector_id.chars().count() > MAX_CONNECTOR_ID_LEN {
            return Err(InvalidScanRequest::new(
                "connector_id",
                format!("connector_id must be at most {MAX_CONNECTOR_ID_LEN} characters"),
            ));
        }
        if let Some(correlation_id) = &self.correlation_id {
            validate_correlation_id(correlation_id)?;
        }
        match &self.content {
            ScanContent::Text(text) => {
                if text.is_empty() {
                    return Err(InvalidScanRequest::new(
                        "payload",
                        "payload must not be empty",
                    ));
                }
                if text.len() > MAX_PAYLOAD_BYTES {
                    return Err(InvalidScanRequest::new(
                        "payload",
                        format!("payload must be at most {MAX_PAYLOAD_BYTES} bytes"),
                    ));
                }
            }
            ScanContent::File(attachment) => {
                if self.content_kind != ContentKind::Upload {
                    return Err(InvalidScanRequest::new(
                        "attachment",
                        "an attachment requires content_kind upload",
                    ));
                }
                Self::validate_attachment(attachment)?;
            }
        }
        self.validate_metadata()
    }

    fn validate_attachment(attachment: &Attachment) -> Result<(), InvalidScanRequest> {
        if attachment.data.is_empty() {
            return Err(InvalidScanRequest::new(
                "attachment",
                "attachment must not be empty",
            ));
        }
        if attachment.data.len() > MAX_ATTACHMENT_BYTES {
            return Err(InvalidScanRequest::new(
                "attachment",
                format!("attachment must be at most {MAX_ATTACHMENT_BYTES} bytes"),
            ));
        }
        if attachment
            .filename
            .as_ref()
            .is_some_and(|n| n.chars().count() > MAX_FILENAME_LEN)
        {
            return Err(InvalidScanRequest::new(
                "attachment.filename",
                format!("filename must be at most {MAX_FILENAME_LEN} characters"),
            ));
        }
        if attachment
            .mime_type
            .as_ref()
            .is_some_and(|m| m.chars().count() > MAX_MIME_LEN)
        {
            return Err(InvalidScanRequest::new(
                "attachment.mime_type",
                format!("mime_type must be at most {MAX_MIME_LEN} characters"),
            ));
        }
        Ok(())
    }

    fn validate_metadata(&self) -> Result<(), InvalidScanRequest> {
        if self.metadata.len() > MAX_METADATA_ENTRIES {
            return Err(InvalidScanRequest::new(
                "metadata",
                format!("metadata must have at most {MAX_METADATA_ENTRIES} entries"),
            ));
        }
        for (key, value) in &self.metadata {
            if key.len() > MAX_METADATA_KEY_LEN {
                return Err(InvalidScanRequest::new(
                    "metadata",
                    format!("metadata keys must be at most {MAX_METADATA_KEY_LEN} bytes"),
                ));
            }
            if value.len() > MAX_METADATA_VALUE_LEN {
                return Err(InvalidScanRequest::new(
                    "metadata",
                    format!("metadata values must be at most {MAX_METADATA_VALUE_LEN} bytes"),
                ));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for ScanRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScanRequest")
            .field("connector_id", &self.connector_id)
            .field("correlation_id", &self.correlation_id)
            .field("content_kind", &self.content_kind)
            .field("content", &self.content)
            .field("metadata", &RedactedMetadata(&self.metadata))
            .finish()
    }
}

/// Check a correlation id: non-blank and at most
/// [`MAX_CORRELATION_ID_LEN`] characters.
///
/// # Errors
///
/// [`InvalidScanRequest`] on field `correlation_id`.
pub fn validate_correlation_id(correlation_id: &str) -> Result<(), InvalidScanRequest> {
    if correlation_id.trim().is_empty() {
        return Err(InvalidScanRequest::new(
            "correlation_id",
            "correlation_id must not be empty",
        ));
    }
    if correlation_id.chars().count() > MAX_CORRELATION_ID_LEN {
        return Err(InvalidScanRequest::new(
            "correlation_id",
            format!("correlation_id must be at most {MAX_CORRELATION_ID_LEN} characters"),
        ));
    }
    Ok(())
}

/// Aggregated verdict for a scanned payload.
///
/// `Debug` prints `redacted_payload` as a length only.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanVerdict {
    /// Unique trace identifier for this scan (used for later lookup / audit).
    pub trace_id: Uuid,
    /// Tenant the scan was performed for.
    pub tenant_id: Uuid,
    /// The aggregated action.
    pub action: VerdictAction,
    /// All findings from all detectors that responded.
    pub findings: Vec<Finding>,
    /// The scanned text with the `Redact` findings' spans replaced (see
    /// [`redact_spans`](crate::redact_spans)).
    ///
    /// Only ever set on the verdict returned by a synchronous
    /// [`scan`](crate::PromptScannerClientV1::scan) whose `action` is
    /// [`VerdictAction::Redact`]. Always `None` on a verdict read back via
    /// `get_verdict`/`find_by_correlation_id`: the payload is never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redacted_payload: Option<String>,
    /// Enabled backends that did not contribute to `action` (degraded
    /// coverage): they timed out, errored, or had no usable instance or
    /// model — only non-required ones, since a required one in that state
    /// fails the scan instead — or they deferred, required or not (their
    /// findings are added to the stored scan later). These never change
    /// `action`; a caller that needs full coverage checks that this is
    /// empty.
    ///
    /// Only set on the verdict returned by a synchronous
    /// [`scan`](crate::PromptScannerClientV1::scan). Not stored: always empty
    /// on a verdict read back via `get_verdict`/`find_by_correlation_id`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unscored_backends: Vec<UnscoredBackend>,
    /// When the scan was performed.
    pub created_at: DateTime<Utc>,
}

/// An enabled backend that did not contribute to a verdict, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct UnscoredBackend {
    /// The backend's vendor, as listed in `TenantPolicy::enabled_backends`.
    pub backend: String,
    /// Why it did not contribute.
    pub reason: UnscoredReason,
}

impl UnscoredBackend {
    /// `backend` (its vendor) did not contribute, for `reason`.
    #[must_use]
    pub fn new(backend: impl Into<String>, reason: UnscoredReason) -> Self {
        Self {
            backend: backend.into(),
            reason,
        }
    }
}

/// Why a backend did not contribute to a verdict: a stable code, safe to
/// show to the caller. The details are only in the gateway's logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum UnscoredReason {
    /// It did not answer within the gateway's `backend_timeout`.
    Timeout,
    /// No usable plugin instance or client was registered, or the backend
    /// reported itself unavailable (e.g. its model is not loaded).
    Unavailable,
    /// It returned an error.
    Error,
    /// It deferred the input; its findings are added to the stored scan
    /// later, in the background.
    Deferred,
}

impl UnscoredReason {
    /// The stable `snake_case` code, as serialized.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
            Self::Error => "error",
            Self::Deferred => "deferred",
        }
    }
}

impl fmt::Debug for ScanVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScanVerdict")
            .field("trace_id", &self.trace_id)
            .field("tenant_id", &self.tenant_id)
            .field("action", &self.action)
            .field("findings", &self.findings)
            .field(
                "redacted_payload",
                &self.redacted_payload.as_ref().map(|p| Redacted(p.len())),
            )
            .field("unscored_backends", &self.unscored_backends)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// Why an asynchronous scan job failed: a stable code, safe to show to the
/// caller. The details are only in the gateway's logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ScanJobFailureCode {
    /// The request failed validation once its content was resolved.
    Validation,
    /// The caller was not allowed to scan.
    Forbidden,
    /// The tenant's policy could not be loaded.
    PolicyUnavailable,
    /// A backend the tenant policy requires failed, timed out or was not
    /// available.
    RequiredBackendFailed,
    /// Extracting text from the attachment failed.
    AttachmentParsingFailed,
    /// The gateway was overloaded.
    Overloaded,
    /// Any other failure.
    Internal,
}

/// Status of an asynchronously-processed scan job, from
/// [`PromptScannerClientV1::get_job_status`](crate::PromptScannerClientV1::get_job_status).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ScanJobStatus {
    /// Queued, not yet picked up by the background worker.
    Queued,
    /// Currently being scanned.
    Running,
    /// Finished. A flagged scan's findings are available via `get_verdict`
    /// with `trace_id`.
    Completed {
        /// The trace id the scan was recorded under.
        trace_id: Uuid,
    },
    /// Failed; no verdict was produced.
    Failed {
        /// Why.
        code: ScanJobFailureCode,
    },
}

/// Handle returned by `scan_async`: poll
/// [`get_job_status`](crate::PromptScannerClientV1::get_job_status) with
/// `job_id`, then fetch a flagged scan's findings with `trace_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanJobHandle {
    /// Unique job identifier.
    pub job_id: Uuid,
    /// The trace id the eventual `ScanVerdict` will be recorded under.
    pub trace_id: Uuid,
    /// Current job status.
    pub status: ScanJobStatus,
}

/// Per-tenant scanner backend configuration.
///
/// Build one with [`TenantPolicy::default_for_tenant`] and set the fields
/// you need: new fields may be added.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TenantPolicy {
    /// Tenant this policy applies to.
    pub tenant_id: Uuid,
    /// Vendor identifiers of scanner backend plugins enabled for this
    /// tenant.
    ///
    /// Matched against the `vendor` field of registered
    /// `ScannerBackendSpecV1` instances in `types-registry`.
    #[serde(default)]
    pub enabled_backends: Vec<String>,
    /// Subset of `enabled_backends` that must respond for the scan to
    /// succeed. If a required backend has no usable instance, times out or
    /// errors, the scan returns an error instead of a verdict
    /// ([`PromptScannerError::BackendUnavailable`](crate::PromptScannerError::BackendUnavailable)
    /// or [`RequiredBackendFailed`](crate::PromptScannerError::RequiredBackendFailed)
    /// → 503, [`BackendTimeout`](crate::PromptScannerError::BackendTimeout)
    /// → 504) rather than degrading to "unscored". A backend not listed here
    /// that fails is listed in [`ScanVerdict::unscored_backends`] and does
    /// not change the verdict.
    #[serde(default)]
    pub required_backends: Vec<String>,
    /// Per-category confidence thresholds below which a finding is ignored
    /// during aggregation. Keyed by `Finding::category`.
    #[serde(default)]
    pub thresholds: HashMap<String, f32>,
    /// Action to take when no finding exceeds its threshold.
    #[serde(default)]
    pub default_action: VerdictAction,
    /// Overrides a finding's own `suggested_action` when a configured key
    /// covers its category: the key equals the category or a whole leading
    /// run of its dot-separated segments (`"pii"` covers `"pii"` and
    /// `"pii.email"`, never `"pii_legacy.x"`). The most specific (longest)
    /// covering key wins.
    /// Applied by `Service::aggregate_verdict` before taking the max action
    /// across findings. Empty by default: detectors' own suggested actions
    /// are authoritative, exactly as before this field existed.
    ///
    /// This is what lets a policy change *what happens* on a finding (not
    /// just whether it counts, per `thresholds`) — e.g. mapping key `"pii"`
    /// (every `pii.*` category) to `Allow` to effectively turn PII
    /// detection's effect off without disabling the backends that produce
    /// those findings (which may also report non-PII categories).
    #[serde(default)]
    pub category_action_overrides: HashMap<String, VerdictAction>,
    /// Content kinds this policy governs. A scan of any other kind runs no
    /// backends and is allowed. Absent means every kind, as before this field
    /// existed.
    #[serde(default = "ContentKind::all")]
    pub content_kinds: Vec<ContentKind>,
    /// How long the findings of a flagged scan are kept. `prompt-scanner`
    /// clamps it to its own `max_retention` (and `max_evidence_retention` for
    /// a finding stored with evidence). Absent means `max_retention`.
    #[serde(
        default,
        with = "humantime_serde",
        skip_serializing_if = "Option::is_none"
    )]
    pub retention: Option<Duration>,
    /// Whether the text around a finding is stored with it. Off by default.
    #[serde(default)]
    pub evidence: EvidencePolicy,
    /// Version of the policy's settings format (GRPM's `settings_schema`,
    /// `<major>.<minor>`), stored with every finding. Absent when the policy
    /// source has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_schema: Option<String>,
}

/// Whether, and for which verdicts, the text around a finding is stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidencePolicy {
    /// Store the text around each finding. Off by default: payloads are
    /// ephemeral unless the tenant opts in.
    #[serde(default)]
    pub enabled: bool,
    /// Store evidence only for scans with one of these verdicts. Absent means
    /// every verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<VerdictAction>>,
}

impl EvidencePolicy {
    /// Whether a scan with verdict `action` stores evidence.
    #[must_use]
    pub fn applies_to(&self, action: VerdictAction) -> bool {
        self.enabled
            && self
                .actions
                .as_ref()
                .is_none_or(|actions| actions.contains(&action))
    }
}

impl TenantPolicy {
    /// A permissive default policy: no backends enabled, default action
    /// `Allow`, every content kind. Used when a tenant has not yet
    /// configured a policy.
    #[must_use]
    pub fn default_for_tenant(tenant_id: Uuid) -> Self {
        Self {
            tenant_id,
            enabled_backends: Vec::new(),
            required_backends: Vec::new(),
            thresholds: HashMap::new(),
            default_action: VerdictAction::Allow,
            category_action_overrides: HashMap::new(),
            content_kinds: ContentKind::all(),
            retention: None,
            evidence: EvidencePolicy::default(),
            settings_schema: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_content_kinds_means_every_kind() {
        let policy: TenantPolicy = serde_json::from_value(serde_json::json!({
            "tenant_id": Uuid::nil(),
        }))
        .unwrap();
        assert_eq!(policy.content_kinds, ContentKind::ALL);

        let policy: TenantPolicy = serde_json::from_value(serde_json::json!({
            "tenant_id": Uuid::nil(),
            "content_kinds": ["prompt", "mcp_tool_call"],
        }))
        .unwrap();
        assert_eq!(
            policy.content_kinds,
            [ContentKind::Prompt, ContentKind::McpToolCall]
        );
    }

    #[test]
    fn retention_and_evidence_default_off_and_parse() {
        let policy: TenantPolicy = serde_json::from_value(serde_json::json!({
            "tenant_id": Uuid::nil(),
        }))
        .unwrap();
        assert_eq!(policy.retention, None);
        assert!(!policy.evidence.applies_to(VerdictAction::Block));

        let policy: TenantPolicy = serde_json::from_value(serde_json::json!({
            "tenant_id": Uuid::nil(),
            "retention": "30d",
            "evidence": {"enabled": true, "actions": ["block"]},
            "settings_schema": "1.0",
        }))
        .unwrap();
        assert_eq!(policy.retention, Some(Duration::from_hours(30 * 24)));
        assert!(policy.evidence.applies_to(VerdictAction::Block));
        assert!(!policy.evidence.applies_to(VerdictAction::Warn));
        assert_eq!(policy.settings_schema.as_deref(), Some("1.0"));
    }

    #[test]
    fn evidence_enabled_without_actions_applies_to_every_verdict() {
        let evidence = EvidencePolicy {
            enabled: true,
            actions: None,
        };
        for action in [
            VerdictAction::Allow,
            VerdictAction::Audit,
            VerdictAction::Warn,
            VerdictAction::Redact,
            VerdictAction::Block,
        ] {
            assert!(evidence.applies_to(action));
        }
    }

    #[test]
    fn debug_never_prints_content_or_metadata_values() {
        let mut req = ScanRequest::text("conn", ContentKind::Prompt, "my-secret-token");
        req.metadata
            .insert("api_key".to_owned(), "sk-live-123".to_owned());
        let printed = format!("{req:?}");
        assert!(!printed.contains("my-secret-token"), "{printed}");
        assert!(!printed.contains("sk-live-123"), "{printed}");
        assert!(printed.contains("api_key"), "{printed}");

        let file = ScanRequest::file(
            "conn",
            Attachment::new(&b"secret-bytes"[..]).with_filename("secret-name.txt"),
        );
        let printed = format!("{file:?}");
        assert!(!printed.contains("secret"), "{printed}");

        let input = ScanInput::new(Uuid::nil(), ContentKind::Prompt, "my-secret-token");
        assert!(!format!("{input:?}").contains("my-secret-token"));
    }

    #[test]
    fn validate_accepts_text_for_every_kind_and_file_for_upload() {
        for kind in ContentKind::ALL {
            assert!(ScanRequest::text("c", kind, "x").validate().is_ok());
        }
        assert!(
            ScanRequest::file("c", Attachment::new(&b"x"[..]))
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn validate_rejects_file_on_non_upload_kind() {
        let mut req = ScanRequest::file("c", Attachment::new(&b"x"[..]));
        req.content_kind = ContentKind::Prompt;
        assert_eq!(req.validate().unwrap_err().field, "attachment");
    }

    #[test]
    fn validate_enforces_limits() {
        assert_eq!(
            ScanRequest::text("c", ContentKind::Prompt, "")
                .validate()
                .unwrap_err()
                .field,
            "payload"
        );
        assert_eq!(
            ScanRequest::text("c", ContentKind::Prompt, "a".repeat(MAX_PAYLOAD_BYTES + 1))
                .validate()
                .unwrap_err()
                .field,
            "payload"
        );
        assert_eq!(
            ScanRequest::text(" ", ContentKind::Prompt, "x")
                .validate()
                .unwrap_err()
                .field,
            "connector_id"
        );
        assert_eq!(
            ScanRequest::file("c", Attachment::new(Bytes::new()))
                .validate()
                .unwrap_err()
                .field,
            "attachment"
        );
        assert_eq!(
            ScanRequest::file(
                "c",
                Attachment::new(&b"x"[..]).with_filename("n".repeat(MAX_FILENAME_LEN + 1))
            )
            .validate()
            .unwrap_err()
            .field,
            "attachment.filename"
        );

        let mut req = ScanRequest::text("c", ContentKind::Prompt, "x");
        req.correlation_id = Some("x".repeat(MAX_CORRELATION_ID_LEN + 1));
        assert_eq!(req.validate().unwrap_err().field, "correlation_id");

        let mut req = ScanRequest::text("c", ContentKind::Prompt, "x");
        for i in 0..=MAX_METADATA_ENTRIES {
            req.metadata.insert(i.to_string(), String::new());
        }
        assert_eq!(req.validate().unwrap_err().field, "metadata");

        let mut req = ScanRequest::text("c", ContentKind::Prompt, "x");
        req.metadata
            .insert("k".to_owned(), "v".repeat(MAX_METADATA_VALUE_LEN + 1));
        assert_eq!(req.validate().unwrap_err().field, "metadata");
    }

    #[test]
    fn finding_from_detection_stamps_identity() {
        let detection = Detection::new("pii.email", Severity::High, 0.9, VerdictAction::Redact)
            .with_span(Span { start: 1, end: 4 });
        let finding = Finding::from_detection(detection, "gts.x~", Some("1.0".to_owned()));
        assert_eq!(finding.detector, "gts.x~");
        assert_eq!(finding.detector_version.as_deref(), Some("1.0"));
        assert_eq!(finding.category, "pii.email");
        assert_eq!(finding.span, Some(Span { start: 1, end: 4 }));
    }

    #[test]
    fn job_status_serializes_with_tag() {
        let status = ScanJobStatus::Failed {
            code: ScanJobFailureCode::PolicyUnavailable,
        };
        assert_eq!(
            serde_json::to_value(status).unwrap(),
            serde_json::json!({"status": "failed", "code": "policy_unavailable"})
        );
    }

    #[test]
    fn scan_input_is_inline_unless_marked_deferred() {
        let input = ScanInput::new(Uuid::nil(), ContentKind::Prompt, "x");
        assert_eq!(input.phase, ScanPhase::Inline);
        assert!(!input.is_deferred_run());
        let retry = input.with_phase(ScanPhase::Deferred);
        assert!(retry.is_deferred_run());

        // A serialized input without `phase` is an inline run.
        let parsed: ScanInput = serde_json::from_value(serde_json::json!({
            "tenant_id": Uuid::nil(),
            "content_kind": "prompt",
            "payload": "x",
            "metadata": {},
        }))
        .unwrap();
        assert_eq!(parsed.phase, ScanPhase::Inline);
        assert_eq!(
            serde_json::to_value(retry).unwrap()["phase"],
            serde_json::json!("deferred")
        );
    }

    #[test]
    fn unscored_reason_codes_are_stable() {
        for (reason, code) in [
            (UnscoredReason::Timeout, "timeout"),
            (UnscoredReason::Unavailable, "unavailable"),
            (UnscoredReason::Error, "error"),
            (UnscoredReason::Deferred, "deferred"),
        ] {
            assert_eq!(reason.as_str(), code);
            assert_eq!(
                serde_json::to_value(reason).unwrap(),
                serde_json::json!(code)
            );
        }
    }

    #[test]
    fn verdict_lists_unscored_backends_only_when_present() {
        let mut verdict = ScanVerdict {
            trace_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            action: VerdictAction::Allow,
            findings: Vec::new(),
            redacted_payload: None,
            unscored_backends: Vec::new(),
            created_at: DateTime::<Utc>::UNIX_EPOCH,
        };
        let json = serde_json::to_value(&verdict).unwrap();
        assert!(json.get("unscored_backends").is_none(), "{json}");

        verdict.unscored_backends = vec![UnscoredBackend::new("onnx", UnscoredReason::Timeout)];
        let json = serde_json::to_value(&verdict).unwrap();
        assert_eq!(
            json["unscored_backends"],
            serde_json::json!([{"backend": "onnx", "reason": "timeout"}])
        );
        let back: ScanVerdict = serde_json::from_value(json).unwrap();
        assert_eq!(back, verdict);
    }
}
