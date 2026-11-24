//! `evidences` row <-> domain model conversions.
//!
//! Enums are stored as their serde `snake_case` tags (via `serde_json`)
//! rather than a hand-rolled match, so the wire format, the DB format and the
//! migration's `CHECK` lists name the same values.

use prompt_scanner_sdk::{Finding, Span};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::domain::error::DomainError;
use crate::domain::repos::{EvidenceRow, Excerpt};
use crate::infra::storage::entity::evidence;

fn tag<T: Serialize>(value: T, what: &str) -> Result<String, DomainError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or_else(|| DomainError::mapping(format!("failed to serialize {what}")))
}

fn untag<T: DeserializeOwned>(s: &str, what: &str) -> Result<T, DomainError> {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
        .map_err(|e| DomainError::mapping(format!("invalid {what} '{s}': {e}")))
}

fn offset_to_db(v: u32, what: &str) -> Result<i32, DomainError> {
    i32::try_from(v).map_err(|_| DomainError::mapping(format!("{what} {v} exceeds INTEGER")))
}

fn offset_from_db(v: i32, what: &str) -> Result<u32, DomainError> {
    u32::try_from(v).map_err(|_| DomainError::mapping(format!("negative {what} {v}")))
}

/// Convert a domain row into an `evidences` model.
pub(crate) fn to_model(row: EvidenceRow) -> Result<evidence::Model, DomainError> {
    let EvidenceRow {
        trace_id,
        ordinal,
        tenant_id,
        correlation_id,
        connector_id,
        content_kind,
        created_at,
        expires_at,
        action,
        settings_schema,
        finding: f,
        excerpt,
    } = row;
    let (span_start, span_end) = match f.span {
        Some(s) => (
            Some(offset_to_db(s.start, "span_start")?),
            Some(offset_to_db(s.end, "span_end")?),
        ),
        None => (None, None),
    };
    let (excerpt, excerpt_start) = match excerpt {
        Some(e) => (Some(e.text), Some(offset_to_db(e.start, "excerpt_start")?)),
        None => (None, None),
    };
    Ok(evidence::Model {
        trace_id,
        ordinal: i16::from(ordinal),
        tenant_id,
        correlation_id,
        connector_id,
        content_kind: tag(content_kind, "content_kind")?,
        created_at,
        expires_at,
        action: tag(action, "action")?,
        settings_schema,
        detector: f.detector,
        detector_version: f.detector_version,
        category: f.category,
        severity: tag(f.severity, "severity")?,
        confidence: f.confidence,
        suggested_action: tag(f.suggested_action, "suggested_action")?,
        span_start,
        span_end,
        excerpt,
        excerpt_start,
    })
}

/// Convert an `evidences` model into the domain row.
pub(crate) fn from_model(m: evidence::Model) -> Result<EvidenceRow, DomainError> {
    let span = match (m.span_start, m.span_end) {
        (Some(start), Some(end)) => Some(Span {
            start: offset_from_db(start, "span_start")?,
            end: offset_from_db(end, "span_end")?,
        }),
        _ => None,
    };
    let excerpt = match (m.excerpt, m.excerpt_start) {
        (Some(text), Some(start)) => Some(Excerpt {
            text,
            start: offset_from_db(start, "excerpt_start")?,
        }),
        _ => None,
    };
    Ok(EvidenceRow {
        trace_id: m.trace_id,
        ordinal: u8::try_from(m.ordinal)
            .map_err(|_| DomainError::mapping(format!("ordinal {} out of range", m.ordinal)))?,
        tenant_id: m.tenant_id,
        correlation_id: m.correlation_id,
        connector_id: m.connector_id,
        content_kind: untag(&m.content_kind, "content_kind")?,
        created_at: m.created_at,
        expires_at: m.expires_at,
        action: untag(&m.action, "action")?,
        settings_schema: m.settings_schema,
        finding: Finding {
            detector: m.detector,
            category: m.category,
            severity: untag(&m.severity, "severity")?,
            confidence: m.confidence,
            span,
            suggested_action: untag(&m.suggested_action, "suggested_action")?,
            detector_version: m.detector_version,
        },
        excerpt,
    })
}

#[cfg(test)]
#[path = "mapper_tests.rs"]
mod mapper_tests;
