use super::*;

fn parse(value: serde_json::Value) -> PromptScannerConfig {
    serde_json::from_value(value).expect("config parses")
}

#[test]
fn fallback_policy_fields_default_like_tenant_policy() {
    let cfg = parse(serde_json::json!({
        "fallback_policy": {"enabled_backends": ["cf.builtin.regex_scanner"]},
    }));
    let policy = cfg.fallback_policy.expect("set").into_template();
    assert_eq!(policy.enabled_backends, ["cf.builtin.regex_scanner"]);
    assert_eq!(policy.default_action, VerdictAction::Allow);
    assert_eq!(policy.content_kinds, ContentKind::ALL);

    let cfg = parse(serde_json::json!({}));
    assert!(cfg.fallback_policy.is_none());
}

#[test]
fn defaults_match_the_design_and_validate() {
    let cfg = parse(serde_json::json!({}));
    assert_eq!(cfg.fail_mode, FailMode::Closed);
    assert_eq!(cfg.backend_timeout, Duration::from_secs(5));
    assert_eq!(cfg.deferred_backend_timeout, Duration::from_secs(60));
    assert_eq!(cfg.attachment_parse_timeout, Duration::from_secs(30));
    assert_eq!(cfg.job_poll_interval, Duration::from_millis(200));
    assert_eq!(cfg.jobs_per_tick, 32);
    assert_eq!(cfg.job_queue_capacity, 1_024);
    assert_eq!(cfg.job_status_ttl, Duration::from_hours(1));
    let e = &cfg.evidence;
    assert_eq!(e.max_retention, DAY * 90);
    assert_eq!(e.max_evidence_retention, DAY * 30);
    assert_eq!(e.evidence_context_chars, 256);
    assert_eq!(e.max_findings_stored, 256);
    assert_eq!(e.retention_interval, Duration::from_mins(5));
    assert_eq!(e.retention_batch_size, 10_000);
    assert_eq!(e.write_queue_capacity, 1_024);
    assert_eq!(e.write_timeout, Duration::from_secs(10));
    cfg.validate().expect("defaults are valid");
}

#[test]
fn durations_take_humantime_and_the_old_millisecond_keys() {
    let cfg = parse(serde_json::json!({
        "backend_timeout": "2s",
        "job_poll_interval": "50ms",
        "fail_mode": "open",
    }));
    assert_eq!(cfg.backend_timeout, Duration::from_secs(2));
    assert_eq!(cfg.job_poll_interval, Duration::from_millis(50));
    assert_eq!(cfg.fail_mode, FailMode::Open);

    let cfg = parse(serde_json::json!({
        "backend_timeout_ms": 750,
        "job_poll_interval_ms": 20,
    }));
    assert_eq!(cfg.backend_timeout, Duration::from_millis(750));
    assert_eq!(cfg.job_poll_interval, Duration::from_millis(20));

    assert!(
        serde_json::from_value::<PromptScannerConfig>(
            serde_json::json!({"backend_timeout": "soon"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<PromptScannerConfig>(serde_json::json!({"unknown_key": 1}))
            .is_err()
    );
}

#[test]
fn a_round_trip_keeps_every_value() {
    let cfg = PromptScannerConfig::default();
    let back: PromptScannerConfig =
        serde_json::from_value(serde_json::to_value(&cfg).expect("serializes")).expect("parses");
    assert_eq!(back.backend_timeout, cfg.backend_timeout);
    assert_eq!(back.job_status_ttl, cfg.job_status_ttl);
    assert_eq!(back.evidence.write_timeout, cfg.evidence.write_timeout);
}

/// Every numeric tunable: zero, out of range or out of order is refused,
/// naming the field.
#[test]
fn invalid_values_are_refused_naming_the_field() {
    let cases: &[(serde_json::Value, &str)] = &[
        (
            serde_json::json!({"backend_timeout": "0s"}),
            "backend_timeout",
        ),
        (
            serde_json::json!({"backend_timeout_ms": 0}),
            "backend_timeout",
        ),
        (
            serde_json::json!({"backend_timeout": "1h"}),
            "backend_timeout",
        ),
        (
            serde_json::json!({"deferred_backend_timeout": "0s"}),
            "deferred_backend_timeout",
        ),
        (
            serde_json::json!({"attachment_parse_timeout": "0s"}),
            "attachment_parse_timeout",
        ),
        (
            serde_json::json!({"job_poll_interval_ms": 0}),
            "job_poll_interval",
        ),
        (serde_json::json!({"jobs_per_tick": 0}), "jobs_per_tick"),
        (
            serde_json::json!({"job_queue_capacity": 0}),
            "job_queue_capacity",
        ),
        (
            serde_json::json!({"job_queue_capacity": 2_000_000}),
            "job_queue_capacity",
        ),
        (
            serde_json::json!({"job_status_ttl": "0s"}),
            "job_status_ttl",
        ),
        (
            serde_json::json!({"evidence": {"max_retention": "0s"}}),
            "evidence.max_retention",
        ),
        (
            serde_json::json!({"evidence": {"max_evidence_retention": "0s"}}),
            "evidence.max_evidence_retention",
        ),
        (
            serde_json::json!({"evidence": {"max_retention": "7d"}}),
            "evidence.max_evidence_retention",
        ),
        (
            serde_json::json!({"evidence": {"evidence_context_chars": 5000}}),
            "evidence.evidence_context_chars",
        ),
        (
            serde_json::json!({"evidence": {"max_findings_stored": 0}}),
            "evidence.max_findings_stored",
        ),
        (
            serde_json::json!({"evidence": {"max_findings_stored": 257}}),
            "evidence.max_findings_stored",
        ),
        (
            serde_json::json!({"evidence": {"retention_interval": "0s"}}),
            "evidence.retention_interval",
        ),
        (
            serde_json::json!({"evidence": {"retention_batch_size": 0}}),
            "evidence.retention_batch_size",
        ),
        (
            serde_json::json!({"evidence": {"write_queue_capacity": 0}}),
            "evidence.write_queue_capacity",
        ),
        (
            serde_json::json!({"evidence": {"write_timeout": "0s"}}),
            "evidence.write_timeout",
        ),
    ];
    for (value, field) in cases {
        let err = parse(value.clone())
            .validate()
            .expect_err(&format!("{value} must be refused"));
        assert_eq!(err.field, *field, "{value}");
    }
}
