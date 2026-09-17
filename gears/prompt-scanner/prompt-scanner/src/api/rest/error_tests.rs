use super::*;
use crate::domain::error::AttachmentFailure;
use prompt_scanner_sdk::PromptScannerError;
use uuid::Uuid;

/// Text that must never reach the wire.
const SECRET: &str = "grpm.internal:8443 password=hunter2";

#[test]
fn policy_unavailable_maps_to_503_without_the_reason() {
    let tenant_id = Uuid::now_v7();
    // As a policy provider returns it, through the domain.
    let err = DomainError::from(PromptScannerError::policy_unavailable(tenant_id, SECRET));
    assert!(matches!(err, DomainError::PolicyUnavailable { tenant_id: t, .. } if t == tenant_id));

    let ce = CanonicalError::from(err);
    assert_eq!(ce.status_code(), 503);
    assert!(ce.detail().contains(&tenant_id.to_string()));
    assert!(!ce.detail().contains("grpm.internal"));
}

/// Every variant: its status, and no internal text on the wire.
#[test]
fn every_domain_error_maps_to_its_status_without_internal_text() {
    let id = Uuid::now_v7();
    let cases: Vec<(DomainError, u16)> = vec![
        (DomainError::verdict_not_found(id), 404),
        (DomainError::job_not_found(id), 404),
        (DomainError::validation("payload", "must not be empty"), 400),
        (DomainError::Forbidden, 403),
        (
            DomainError::BackendTimeout {
                backend: SECRET.to_owned(),
            },
            504,
        ),
        (
            DomainError::BackendUnavailable {
                backend: "b".to_owned(),
                reason: SECRET.to_owned(),
            },
            503,
        ),
        (
            DomainError::RequiredBackendFailed {
                backend: "b".to_owned(),
                reason: SECRET.to_owned(),
            },
            503,
        ),
        (
            DomainError::PolicyUnavailable {
                tenant_id: id,
                reason: SECRET.to_owned(),
            },
            503,
        ),
        (DomainError::Overloaded, 503),
        (
            DomainError::AttachmentParsingFailed {
                kind: AttachmentFailure::Unparseable,
                reason: SECRET.to_owned(),
            },
            400,
        ),
        (DomainError::unsupported("put_policy"), 501),
        (
            DomainError::database("insert", std::io::Error::other(SECRET)),
            500,
        ),
        (DomainError::Internal(SECRET.to_owned()), 500),
    ];
    for (err, status) in cases {
        let shown = format!("{err}");
        let ce = CanonicalError::from(err);
        assert_eq!(ce.status_code(), status, "{shown}");
        let wire = serde_json::to_string(&Problem::from(ce)).expect("serializes");
        assert!(!wire.contains("hunter2"), "{shown} leaked: {wire}");
    }
}

#[test]
fn file_parser_outages_are_ours_and_bad_files_are_the_callers() {
    use file_parser_sdk::FileParserClientError as E;
    let outage = DomainError::from(E::Internal("disk on fire".to_owned()));
    assert_eq!(CanonicalError::from(outage).status_code(), 500);
    for bad in [
        E::UnsupportedFileType {
            extension: "exe".to_owned(),
        },
        E::InvalidRequest {
            message: "too big".to_owned(),
        },
        E::ParseFailed {
            message: "corrupt".to_owned(),
        },
    ] {
        let err = DomainError::from(bad);
        assert!(matches!(err, DomainError::AttachmentParsingFailed { .. }));
        assert_eq!(CanonicalError::from(err).status_code(), 400);
    }
}

#[test]
fn a_scope_refusal_is_forbidden_not_a_database_error() {
    use toolkit_db::secure::ScopeError;
    assert!(matches!(
        DomainError::from(ScopeError::Denied("nope")),
        DomainError::Forbidden
    ));
    assert!(matches!(
        DomainError::from(ScopeError::TenantNotInScope {
            tenant_id: Uuid::nil()
        }),
        DomainError::Forbidden
    ));
    assert!(matches!(
        DomainError::from(ScopeError::Invalid("bad")),
        DomainError::Database { .. }
    ));
}
