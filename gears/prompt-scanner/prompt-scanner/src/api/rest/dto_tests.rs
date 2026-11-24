#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "tests: a failed expectation is the test failing"
)]

use super::*;

fn scan_dto(payload: &str, attachment: Option<AttachmentDto>) -> ScanRequestDto {
    ScanRequestDto {
        connector_id: "c".to_owned(),
        correlation_id: None,
        content_kind: ContentKindDto::Upload,
        payload: payload.to_owned(),
        metadata: HashMap::new(),
        attachment,
    }
}

fn attachment(data_base64: String) -> AttachmentDto {
    AttachmentDto {
        filename: Some("note.txt".to_owned()),
        mime_type: Some("text/plain".to_owned()),
        data_base64,
    }
}

/// Invalid base64 is refused by the REST DTO conversion (base64 exists only
/// on the wire), not passed on as an empty payload.
#[test]
fn invalid_base64_is_rejected() {
    let err = ScanRequest::try_from(scan_dto("", Some(attachment("not-base64!!!".to_owned()))))
        .expect_err("refused");
    assert!(matches!(err, DomainError::Validation { .. }));
}

#[test]
fn oversized_base64_is_rejected_before_decoding() {
    let err = ScanRequest::try_from(scan_dto(
        "",
        Some(attachment("A".repeat(MAX_ATTACHMENT_BASE64_LEN + 4))),
    ))
    .expect_err("refused");
    assert!(matches!(err, DomainError::Validation { ref field, .. } if field == "attachment"));
}

#[test]
fn payload_and_attachment_together_are_rejected() {
    let err = ScanRequest::try_from(scan_dto("text", Some(attachment("aGk=".to_owned()))))
        .expect_err("refused");
    assert!(matches!(err, DomainError::Validation { ref field, .. } if field == "payload"));

    let ok = ScanRequest::try_from(scan_dto("", Some(attachment("aGk=".to_owned())))).unwrap();
    assert!(matches!(ok.content, ScanContent::File(ref a) if a.data.as_ref() == b"hi"));
}

#[test]
fn every_enum_dto_round_trips_through_the_sdk_type() {
    for kind in ContentKind::ALL {
        let dto = ContentKindDto::from(kind);
        let json = serde_json::to_value(dto).unwrap();
        assert_eq!(json, serde_json::to_value(kind).unwrap(), "same wire tag");
        assert_eq!(
            ContentKind::from(serde_json::from_value::<ContentKindDto>(json).unwrap()),
            kind
        );
    }
    for action in [
        VerdictAction::Allow,
        VerdictAction::Audit,
        VerdictAction::Warn,
        VerdictAction::Redact,
        VerdictAction::Block,
    ] {
        let dto = VerdictActionDto::from(action);
        let json = serde_json::to_value(dto).unwrap();
        assert_eq!(json, serde_json::to_value(action).unwrap());
        assert_eq!(
            VerdictAction::from(serde_json::from_value::<VerdictActionDto>(json).unwrap()),
            action
        );
    }
    for severity in [
        Severity::Low,
        Severity::Medium,
        Severity::High,
        Severity::Critical,
    ] {
        assert_eq!(
            serde_json::to_value(SeverityDto::from(severity)).unwrap(),
            serde_json::to_value(severity).unwrap()
        );
    }
}

#[test]
fn job_status_dto_carries_the_trace_or_the_failure_code() {
    let job = Uuid::now_v7();
    let trace = Uuid::now_v7();
    let done = serde_json::to_value(ScanJobStatusResponseDto::new(
        job,
        ScanJobStatus::Completed { trace_id: trace },
    ))
    .unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(done["trace_id"], trace.to_string());
    assert!(done.get("failure_code").is_none());

    let failed = serde_json::to_value(ScanJobStatusResponseDto::new(
        job,
        ScanJobStatus::Failed {
            code: ScanJobFailureCode::PolicyUnavailable,
        },
    ))
    .unwrap();
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["failure_code"], "policy_unavailable");

    let queued =
        serde_json::to_value(ScanJobStatusResponseDto::new(job, ScanJobStatus::Queued)).unwrap();
    assert_eq!(queued["status"], "queued");
    assert!(queued.get("trace_id").is_none());
}

fn full_policy() -> TenantPolicy {
    let mut p = TenantPolicy::default_for_tenant(Uuid::now_v7());
    p.enabled_backends = vec!["a".to_owned(), "b".to_owned()];
    p.required_backends = vec!["a".to_owned()];
    p.thresholds.insert("pii.email".to_owned(), 0.5);
    p.default_action = VerdictAction::Warn;
    p.category_action_overrides
        .insert("pii".to_owned(), VerdictAction::Redact);
    p.content_kinds = vec![ContentKind::Prompt, ContentKind::Upload];
    p.retention = Some(Duration::from_hours(24 * 7));
    p.evidence = EvidencePolicy {
        enabled: true,
        actions: Some(vec![VerdictAction::Block]),
    };
    p.settings_schema = Some("1.0".to_owned());
    p
}

/// What `GET /policy` returns, `PUT /policy` accepts back unchanged.
#[test]
fn a_policy_round_trips_from_get_to_put() {
    let policy = full_policy();
    let json = serde_json::to_value(TenantPolicyDto::from(policy.clone())).unwrap();
    assert_eq!(json["retention"], "7days");
    let back: TenantPolicyUpdateDto = serde_json::from_value(json).unwrap();
    assert_eq!(TenantPolicy::from(back), policy);
}

/// `PUT /policy` needs neither `tenant_id` nor `default_action`.
#[test]
fn a_minimal_put_body_takes_the_sdk_defaults() {
    let dto: TenantPolicyUpdateDto =
        serde_json::from_value(serde_json::json!({"enabled_backends": ["a"]})).unwrap();
    let policy = TenantPolicy::from(dto);
    assert!(policy.tenant_id.is_nil(), "filled in by the service");
    let mut expected = TenantPolicy::default_for_tenant(Uuid::nil());
    expected.enabled_backends = vec!["a".to_owned()];
    assert_eq!(policy, expected);
}

#[test]
fn verdict_dto_lists_unscored_backends_with_stable_codes() {
    let mut verdict = ScanVerdict {
        trace_id: Uuid::nil(),
        tenant_id: Uuid::nil(),
        action: VerdictAction::Allow,
        findings: Vec::new(),
        redacted_payload: None,
        unscored_backends: Vec::new(),
        created_at: DateTime::<Utc>::UNIX_EPOCH,
    };
    let clean = serde_json::to_value(ScanVerdictDto::from(verdict.clone())).unwrap();
    assert!(clean.get("unscored_backends").is_none(), "{clean}");

    verdict.unscored_backends = vec![
        UnscoredBackend::new("onnx", UnscoredReason::Timeout),
        UnscoredBackend::new("regex", UnscoredReason::Unavailable),
        UnscoredBackend::new("dlp", UnscoredReason::Error),
        UnscoredBackend::new("big", UnscoredReason::Deferred),
    ];
    let degraded = serde_json::to_value(ScanVerdictDto::from(verdict)).unwrap();
    assert_eq!(
        degraded["unscored_backends"],
        serde_json::json!([
            {"backend": "onnx", "reason": "timeout"},
            {"backend": "regex", "reason": "unavailable"},
            {"backend": "dlp", "reason": "error"},
            {"backend": "big", "reason": "deferred"},
        ])
    );
}
