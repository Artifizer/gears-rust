//! REST DTOs (serde + utoipa) for the prompt-scanner gear.

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine as _;
use chrono::{DateTime, Utc};
use prompt_scanner_sdk::{
    Attachment, ContentKind, EvidencePolicy, Finding, MAX_ATTACHMENT_BYTES, ScanContent,
    ScanJobFailureCode, ScanJobHandle, ScanJobStatus, ScanRequest, ScanVerdict, Severity, Span,
    TenantPolicy, UnscoredBackend, UnscoredReason, VerdictAction,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Wire representation of [`ContentKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentKindDto {
    Prompt,
    Response,
    Upload,
    McpToolCall,
    McpToolResult,
}

impl From<ContentKind> for ContentKindDto {
    fn from(k: ContentKind) -> Self {
        match k {
            ContentKind::Prompt => Self::Prompt,
            ContentKind::Response => Self::Response,
            ContentKind::Upload => Self::Upload,
            ContentKind::McpToolCall => Self::McpToolCall,
            ContentKind::McpToolResult => Self::McpToolResult,
        }
    }
}

impl From<ContentKindDto> for ContentKind {
    fn from(k: ContentKindDto) -> Self {
        match k {
            ContentKindDto::Prompt => Self::Prompt,
            ContentKindDto::Response => Self::Response,
            ContentKindDto::Upload => Self::Upload,
            ContentKindDto::McpToolCall => Self::McpToolCall,
            ContentKindDto::McpToolResult => Self::McpToolResult,
        }
    }
}

/// Wire representation of [`VerdictAction`].
///
/// Ordered the same as the SDK's `VerdictAction` (see
/// `prompt_scanner_sdk::models::VerdictAction` docs for the precedence rule
/// this ordering encodes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerdictActionDto {
    Allow,
    Audit,
    Warn,
    Redact,
    Block,
}

impl From<VerdictAction> for VerdictActionDto {
    fn from(a: VerdictAction) -> Self {
        match a {
            VerdictAction::Allow => Self::Allow,
            VerdictAction::Audit => Self::Audit,
            VerdictAction::Warn => Self::Warn,
            VerdictAction::Redact => Self::Redact,
            VerdictAction::Block => Self::Block,
        }
    }
}

impl From<VerdictActionDto> for VerdictAction {
    fn from(a: VerdictActionDto) -> Self {
        match a {
            VerdictActionDto::Allow => Self::Allow,
            VerdictActionDto::Audit => Self::Audit,
            VerdictActionDto::Warn => Self::Warn,
            VerdictActionDto::Redact => Self::Redact,
            VerdictActionDto::Block => Self::Block,
        }
    }
}

/// Wire representation of [`Severity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SeverityDto {
    Low,
    Medium,
    High,
    Critical,
}

impl From<Severity> for SeverityDto {
    fn from(s: Severity) -> Self {
        match s {
            Severity::Low => Self::Low,
            Severity::Medium => Self::Medium,
            Severity::High => Self::High,
            Severity::Critical => Self::Critical,
        }
    }
}

/// Wire representation of [`Span`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpanDto {
    pub start: u32,
    pub end: u32,
}

impl From<Span> for SpanDto {
    fn from(s: Span) -> Self {
        Self {
            start: s.start,
            end: s.end,
        }
    }
}

/// Wire representation of a detector [`Finding`].
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FindingDto {
    pub detector: String,
    pub category: String,
    pub severity: SeverityDto,
    pub confidence: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<SpanDto>,
    pub suggested_action: VerdictActionDto,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detector_version: Option<String>,
}

impl From<Finding> for FindingDto {
    fn from(f: Finding) -> Self {
        Self {
            detector: f.detector,
            category: f.category,
            severity: f.severity.into(),
            confidence: f.confidence,
            span: f.span.map(Into::into),
            suggested_action: f.suggested_action.into(),
            detector_version: f.detector_version,
        }
    }
}

/// Wire representation of a raw [`Attachment`] submitted under
/// `content_kind: "upload"`. `prompt-scanner` extracts text from it via
/// `file-parser` before scanning — see `domain::service::Service::
/// resolve_effective_payload`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct AttachmentDto {
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    /// Base64-encoded (standard alphabet, padded) raw file bytes.
    pub data_base64: String,
}

impl TryFrom<AttachmentDto> for Attachment {
    type Error = DomainError;

    /// Decodes the base64 body. Only the REST layer knows about base64. An
    /// encoding longer than [`MAX_ATTACHMENT_BYTES`] can decode to is refused
    /// before decoding.
    fn try_from(dto: AttachmentDto) -> Result<Self, Self::Error> {
        if dto.data_base64.len() > MAX_ATTACHMENT_BASE64_LEN {
            return Err(DomainError::validation(
                "attachment",
                format!("attachment must be at most {MAX_ATTACHMENT_BYTES} bytes"),
            ));
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(dto.data_base64.as_bytes())
            .map_err(|_| DomainError::validation("attachment", "invalid base64"))?;
        let mut attachment = Self::new(data);
        attachment.filename = dto.filename;
        attachment.mime_type = dto.mime_type;
        Ok(attachment)
    }
}

/// Longest base64 text that can decode to at most [`MAX_ATTACHMENT_BYTES`].
const MAX_ATTACHMENT_BASE64_LEN: usize = MAX_ATTACHMENT_BYTES.div_ceil(3) * 4;

/// `POST /prompt-scanner/v1/scans` (and `/scans/async`) request body.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct ScanRequestDto {
    /// At most 255 characters.
    pub connector_id: String,
    /// The consumer's id for this request, at most 255 characters. Stored
    /// with a flagged scan's findings for correlation lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    pub content_kind: ContentKindDto,
    /// The text to scan. Leave it empty (or out) when sending an
    /// `attachment`: the two cannot be combined.
    #[serde(default)]
    pub payload: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    /// Only meaningful when `content_kind == "upload"`. See [`AttachmentDto`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<AttachmentDto>,
}

impl TryFrom<ScanRequestDto> for ScanRequest {
    type Error = DomainError;

    /// Exactly one of `payload` and `attachment`: both is refused rather
    /// than one silently winning.
    fn try_from(dto: ScanRequestDto) -> Result<Self, Self::Error> {
        let content = match dto.attachment {
            Some(_) if !dto.payload.is_empty() => {
                return Err(DomainError::validation(
                    "payload",
                    "send either payload or attachment, not both",
                ));
            }
            Some(attachment) => ScanContent::File(attachment.try_into()?),
            None => ScanContent::Text(dto.payload),
        };
        Ok(Self {
            connector_id: dto.connector_id,
            correlation_id: dto.correlation_id,
            content_kind: dto.content_kind.into(),
            content,
            metadata: dto.metadata,
        })
    }
}

/// `GET /prompt-scanner/v1/scans` query: correlation lookup.
#[derive(Debug, Clone, Deserialize)]
pub struct CorrelationQuery {
    pub correlation_id: String,
}

/// `DELETE /prompt-scanner/v1/scans/{trace_id}` response body.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct EvidenceDeletedDto {
    pub trace_id: Uuid,
    /// Finding rows removed.
    pub deleted: u64,
}

/// Wire representation of [`UnscoredReason`]: a stable code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnscoredReasonDto {
    Timeout,
    Unavailable,
    Error,
    Deferred,
}

impl From<UnscoredReason> for UnscoredReasonDto {
    fn from(r: UnscoredReason) -> Self {
        match r {
            UnscoredReason::Timeout => Self::Timeout,
            UnscoredReason::Unavailable => Self::Unavailable,
            UnscoredReason::Deferred => Self::Deferred,
            // `UnscoredReason` is non-exhaustive; anything new is reported
            // as an error until the DTO learns about it.
            _ => Self::Error,
        }
    }
}

/// An enabled backend that did not contribute to the verdict, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UnscoredBackendDto {
    /// The backend's vendor, as in the policy's `enabled_backends`.
    pub backend: String,
    pub reason: UnscoredReasonDto,
}

impl From<UnscoredBackend> for UnscoredBackendDto {
    fn from(u: UnscoredBackend) -> Self {
        Self {
            backend: u.backend,
            reason: u.reason.into(),
        }
    }
}

/// `POST /prompt-scanner/v1/scans` response body; also a stored scan read
/// back, which never carries `redacted_payload` or `unscored_backends`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ScanVerdictDto {
    pub trace_id: Uuid,
    pub tenant_id: Uuid,
    pub action: VerdictActionDto,
    pub findings: Vec<FindingDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted_payload: Option<String>,
    /// Non-required enabled backends that did not contribute to `action`
    /// (degraded coverage). Absent when every enabled backend contributed.
    /// They never change `action`; a caller that needs full coverage checks
    /// that this is absent. Only on the synchronous scan response: not
    /// stored.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unscored_backends: Vec<UnscoredBackendDto>,
    #[schema(format = "date-time")]
    pub created_at: DateTime<Utc>,
}

impl From<ScanVerdict> for ScanVerdictDto {
    fn from(v: ScanVerdict) -> Self {
        Self {
            trace_id: v.trace_id,
            tenant_id: v.tenant_id,
            action: v.action.into(),
            findings: v.findings.into_iter().map(Into::into).collect(),
            redacted_payload: v.redacted_payload,
            unscored_backends: v.unscored_backends.into_iter().map(Into::into).collect(),
            created_at: v.created_at,
        }
    }
}

/// Wire representation of [`ScanJobStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanJobStatusDto {
    Queued,
    Running,
    Completed,
    Failed,
}

impl From<ScanJobStatus> for ScanJobStatusDto {
    fn from(s: ScanJobStatus) -> Self {
        match s {
            ScanJobStatus::Queued => Self::Queued,
            ScanJobStatus::Running => Self::Running,
            ScanJobStatus::Completed { .. } => Self::Completed,
            // `ScanJobStatus` is non-exhaustive; anything new is reported as
            // failed until the DTO learns about it.
            _ => Self::Failed,
        }
    }
}

/// `POST /prompt-scanner/v1/scans/async` response body.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ScanJobHandleDto {
    pub job_id: Uuid,
    pub trace_id: Uuid,
    pub status: ScanJobStatusDto,
}

impl From<ScanJobHandle> for ScanJobHandleDto {
    fn from(h: ScanJobHandle) -> Self {
        Self {
            job_id: h.job_id,
            trace_id: h.trace_id,
            status: h.status.into(),
        }
    }
}

/// `GET /prompt-scanner/v1/scans/async/{job_id}` response body.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ScanJobStatusResponseDto {
    pub job_id: Uuid,
    pub status: ScanJobStatusDto,
    /// Set when `completed`: fetch a flagged scan's findings with it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<Uuid>,
    /// Set when `failed`: a stable code (`validation`, `forbidden`,
    /// `policy_unavailable`, `required_backend_failed`,
    /// `attachment_parsing_failed`, `overloaded`, `internal`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
}

impl ScanJobStatusResponseDto {
    #[must_use]
    pub fn new(job_id: Uuid, status: ScanJobStatus) -> Self {
        let (trace_id, failure_code) = match status {
            ScanJobStatus::Completed { trace_id } => (Some(trace_id), None),
            ScanJobStatus::Failed { code } => (None, Some(failure_code(code))),
            _ => (None, None),
        };
        Self {
            job_id,
            status: status.into(),
            trace_id,
            failure_code,
        }
    }
}

/// The `snake_case` tag of `code`.
fn failure_code(code: ScanJobFailureCode) -> String {
    serde_json::to_value(code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "internal".to_owned())
}

/// `GET /prompt-scanner/v1/policy` response body.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct TenantPolicyDto {
    pub tenant_id: Uuid,
    #[serde(default)]
    pub enabled_backends: Vec<String>,
    #[serde(default)]
    pub required_backends: Vec<String>,
    #[serde(default)]
    pub thresholds: HashMap<String, f32>,
    pub default_action: VerdictActionDto,
    /// Category-prefix overrides for a finding's suggested action (e.g.
    /// `"pii"` -> `redact`). See `TenantPolicy::category_action_overrides`.
    #[serde(default)]
    pub category_action_overrides: HashMap<String, VerdictActionDto>,
    /// Content kinds this policy governs; absent means every kind. See
    /// `TenantPolicy::content_kinds`.
    #[serde(default = "all_content_kinds")]
    pub content_kinds: Vec<ContentKindDto>,
    /// How long a flagged scan's findings are kept, e.g. `"30d"`. Clamped to
    /// the gear's `max_retention`; absent means that maximum.
    #[serde(
        default,
        with = "humantime_serde",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, example = "30d")]
    pub retention: Option<Duration>,
    /// Whether the text around a finding is stored. Off by default.
    #[serde(default)]
    pub evidence: EvidencePolicyDto,
    /// Version of the policy's settings format, e.g. `"1.0"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_schema: Option<String>,
}

/// Wire representation of [`EvidencePolicy`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct EvidencePolicyDto {
    #[serde(default)]
    pub enabled: bool,
    /// Store evidence only for these verdicts; absent means every verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<VerdictActionDto>>,
}

impl From<EvidencePolicy> for EvidencePolicyDto {
    fn from(e: EvidencePolicy) -> Self {
        Self {
            enabled: e.enabled,
            actions: e
                .actions
                .map(|actions| actions.into_iter().map(Into::into).collect()),
        }
    }
}

impl From<EvidencePolicyDto> for EvidencePolicy {
    fn from(dto: EvidencePolicyDto) -> Self {
        Self {
            enabled: dto.enabled,
            actions: dto
                .actions
                .map(|actions| actions.into_iter().map(Into::into).collect()),
        }
    }
}

fn all_content_kinds() -> Vec<ContentKindDto> {
    ContentKind::ALL.into_iter().map(Into::into).collect()
}

impl From<TenantPolicy> for TenantPolicyDto {
    fn from(p: TenantPolicy) -> Self {
        Self {
            tenant_id: p.tenant_id,
            enabled_backends: p.enabled_backends,
            required_backends: p.required_backends,
            thresholds: p.thresholds,
            default_action: p.default_action.into(),
            category_action_overrides: p
                .category_action_overrides
                .into_iter()
                .map(|(k, v)| (k, v.into()))
                .collect(),
            content_kinds: p.content_kinds.into_iter().map(Into::into).collect(),
            retention: p.retention,
            evidence: p.evidence.into(),
            settings_schema: p.settings_schema,
        }
    }
}

/// `PUT /prompt-scanner/v1/policy` request body: a [`TenantPolicyDto`]
/// whose `tenant_id` may be left out (it is always the caller's; another is
/// refused) and whose `default_action` defaults to `allow`, as in the SDK.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct TenantPolicyUpdateDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<Uuid>,
    #[serde(default)]
    pub enabled_backends: Vec<String>,
    #[serde(default)]
    pub required_backends: Vec<String>,
    #[serde(default)]
    pub thresholds: HashMap<String, f32>,
    #[serde(default = "default_action")]
    pub default_action: VerdictActionDto,
    #[serde(default)]
    pub category_action_overrides: HashMap<String, VerdictActionDto>,
    #[serde(default = "all_content_kinds")]
    pub content_kinds: Vec<ContentKindDto>,
    #[serde(
        default,
        with = "humantime_serde",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>, example = "30d")]
    pub retention: Option<Duration>,
    #[serde(default)]
    pub evidence: EvidencePolicyDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_schema: Option<String>,
}

fn default_action() -> VerdictActionDto {
    VerdictAction::default().into()
}

impl From<TenantPolicyUpdateDto> for TenantPolicy {
    fn from(dto: TenantPolicyUpdateDto) -> Self {
        let mut policy = Self::default_for_tenant(dto.tenant_id.unwrap_or_else(Uuid::nil));
        policy.enabled_backends = dto.enabled_backends;
        policy.required_backends = dto.required_backends;
        policy.thresholds = dto.thresholds;
        policy.default_action = dto.default_action.into();
        policy.category_action_overrides = dto
            .category_action_overrides
            .into_iter()
            .map(|(k, v)| (k, v.into()))
            .collect();
        policy.content_kinds = dto.content_kinds.into_iter().map(Into::into).collect();
        policy.retention = dto.retention;
        policy.evidence = dto.evidence.into();
        policy.settings_schema = dto.settings_schema;
        policy
    }
}

/// `GET /prompt-scanner/v1/backends` response entry.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct BackendInstanceDto {
    /// GTS instance id of the scanner backend plugin.
    pub gts_id: String,
    /// Vendor identifier (matched against `TenantPolicy.enabled_backends`).
    pub vendor: String,
    /// Selection priority as registered (informational only — prompt-scanner
    /// fans out to all matching instances rather than picking by priority).
    pub priority: i16,
    /// Whether a scoped `ClientHub` entry is currently registered for this
    /// instance (i.e. whether the gateway could actually call it right now).
    pub available: bool,
}

#[cfg(test)]
#[path = "dto_tests.rs"]
mod dto_tests;
