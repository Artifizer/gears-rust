#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration test: a failed expectation is the test failing"
)]
//! The `evidences` migration on a real `PostgreSQL`: the backend every stand
//! runs on, and the one the `sqlite::memory:` suites cannot stand in for.
//! The migration carries a separate `PostgreSQL` DDL, which nothing else
//! runs.
//!
//! Docker required: without it the suite skips, unless
//! `PROMPT_SCANNER_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! should set it. Run with:
//!
//! ```sh
//! cargo nextest run -p cf-gears-prompt-scanner --features integration --test pg_migrations_test
//! ```

use std::sync::Arc;

use chrono::{TimeDelta, Utc};
use prompt_scanner::domain::repos::{
    EvidenceMaintenance, EvidenceRepository, EvidenceRow, Excerpt,
};
use prompt_scanner::infra::storage::OrmEvidenceRepository;
use prompt_scanner::infra::storage::migrations::Migrator;
use prompt_scanner::{ContentKind, Finding, Severity, Span, VerdictAction};
use sea_orm::ConnectionTrait;
use sea_orm_migration::MigratorTrait;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use toolkit_security::AccessScope;
use uuid::Uuid;

fn require_docker() -> bool {
    std::env::var_os("PROMPT_SCANNER_PG_REQUIRE_DOCKER").is_some_and(|v| v != "0" && !v.is_empty())
}

fn skip<T>(why: &str) -> Option<T> {
    assert!(!require_docker(), "PostgreSQL migration suite: {why}");
    eprintln!("skipping -- PostgreSQL migration suite: {why}");
    None
}

struct Pg {
    _container: ContainerAsync<Postgres>,
    db: Arc<DBProvider<DbError>>,
    url: String,
}

async fn pg() -> Option<Pg> {
    let container = match test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "scanner")
        .start()
        .await
    {
        Ok(c) => c,
        Err(e) => return skip(&format!("could not start a PostgreSQL container ({e})")),
    };
    let port = match container.get_host_port_ipv4(5432).await {
        Ok(p) => p,
        Err(e) => return skip(&format!("container port unresolved ({e})")),
    };
    let opts = ConnectOpts {
        max_conns: Some(4),
        min_conns: Some(1),
        ..Default::default()
    };
    let url = format!("postgres://user:pass@127.0.0.1:{port}/scanner");
    let db = connect_db(&url, opts).await.expect("connect");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .map_err(|e| e.to_string())
        .expect("the migration applies on PostgreSQL");
    Some(Pg {
        _container: container,
        db: Arc::new(DBProvider::new(db)),
        url,
    })
}

fn row(tenant_id: Uuid, ordinal: u8, expired: bool) -> EvidenceRow {
    let now = Utc::now();
    let created_at = if expired {
        now - TimeDelta::days(100)
    } else {
        now
    };
    EvidenceRow {
        trace_id: Uuid::now_v7(),
        ordinal,
        tenant_id,
        correlation_id: Some("corr-1".to_owned()),
        connector_id: "conn".to_owned(),
        content_kind: ContentKind::McpToolResult,
        created_at,
        expires_at: created_at + TimeDelta::days(30),
        action: VerdictAction::Redact,
        settings_schema: Some("1.0".to_owned()),
        finding: Finding {
            detector: "gts.cf.test.detector.v1~".to_owned(),
            category: "pii.email".to_owned(),
            severity: Severity::Critical,
            confidence: 1.0,
            span: Some(Span { start: 0, end: 3 }),
            suggested_action: VerdictAction::Redact,
            detector_version: Some("1.2.3".to_owned()),
        },
        excerpt: Some(Excerpt {
            text: "ab\u{1F600}".to_owned(),
            start: 0,
        }),
    }
}

#[tokio::test]
async fn the_migration_and_the_repository_work_on_postgres() {
    let Some(pg) = pg().await else {
        return;
    };
    let conn = pg.db.conn().expect("conn");
    let repo = OrmEvidenceRepository::new();
    let tenant = Uuid::now_v7();
    let scope = AccessScope::for_tenant(tenant);

    let live = row(tenant, 0, false);
    repo.insert_all(&conn, &scope, vec![live.clone()])
        .await
        .expect("insert");
    let read = repo
        .find_by_trace(&conn, &scope, live.trace_id, Utc::now())
        .await
        .expect("read");
    assert_eq!(read, vec![live.clone()], "every column round-trips");
    assert_eq!(
        repo.find_by_correlation(&conn, &scope, "corr-1", Utc::now(), 10)
            .await
            .expect("correlation")
            .len(),
        1
    );

    for ordinal in 0..3 {
        repo.insert_all(&conn, &scope, vec![row(tenant, ordinal, true)])
            .await
            .expect("insert expired");
    }
    let all = AccessScope::allow_all();
    assert_eq!(
        repo.count_expired(&conn, &all, Utc::now()).await.unwrap(),
        3
    );
    assert_eq!(
        repo.delete_expired(&conn, &all, Utc::now(), 2)
            .await
            .unwrap(),
        2,
        "LIMIT applies"
    );
    assert_eq!(
        repo.delete_tenant(&conn, &all, tenant, 10).await.unwrap(),
        2
    );
}

#[tokio::test]
async fn the_checks_refuse_what_the_domain_cannot_hold() {
    let Some(pg) = pg().await else {
        return;
    };
    let raw = sea_orm::Database::connect(&pg.url)
        .await
        .expect("plain connection");
    let bad_values = [
        ("content_kind", "'video'"),
        ("action", "'explode'"),
        ("severity", "'meh'"),
        ("confidence", "1.5"),
        ("ordinal", "256"),
    ];
    for (column, value) in bad_values {
        let mut values = vec![
            ("trace_id", format!("'{}'", Uuid::now_v7())),
            ("ordinal", "0".to_owned()),
            ("tenant_id", format!("'{}'", Uuid::now_v7())),
            ("connector_id", "'c'".to_owned()),
            ("content_kind", "'prompt'".to_owned()),
            ("created_at", "now()".to_owned()),
            ("expires_at", "now() + interval '1 day'".to_owned()),
            ("action", "'block'".to_owned()),
            ("detector", "'d'".to_owned()),
            ("category", "'c'".to_owned()),
            ("severity", "'low'".to_owned()),
            ("confidence", "0.5".to_owned()),
            ("suggested_action", "'block'".to_owned()),
        ];
        for v in &mut values {
            if v.0 == column {
                v.1 = value.to_owned();
            }
        }
        let (cols, vals): (Vec<_>, Vec<_>) = values.into_iter().unzip();
        let sql = format!(
            "INSERT INTO evidences ({}) VALUES ({})",
            cols.join(", "),
            vals.join(", ")
        );
        assert!(
            raw.execute_unprepared(&sql).await.is_err(),
            "{column} = {value} must be refused"
        );
    }
}
