//! The gear's initial migration: the `evidences` table.
//!
//! One row per finding of a flagged scan. The PostgreSQL DDL is the reviewed
//! design (`docs/DESIGN.md`, "Storage"); `SQLite` gets the same shape for
//! tests and single-process deployments. Plain `CREATE` statements, without
//! `IF NOT EXISTS`: the migration history table decides whether this runs.
//!
//! The value lists in the `CHECK`s are the `snake_case` tags of the SDK enums
//! (`ContentKind`, `VerdictAction`, `Severity`). A new variant needs a
//! migration that widens the constraint. Retention limits are configuration,
//! so they are not in a `CHECK`.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DbErr};

#[derive(DeriveMigrationName)]
pub struct Migration;

pub const POSTGRES: &str = r#"
CREATE TABLE evidences (
    trace_id         UUID          NOT NULL,
    ordinal          SMALLINT      NOT NULL,
    tenant_id        UUID          NOT NULL,
    correlation_id   VARCHAR(255),
    connector_id     VARCHAR(255)  NOT NULL,
    content_kind     VARCHAR(32)   NOT NULL,
    created_at       TIMESTAMPTZ   NOT NULL,
    expires_at       TIMESTAMPTZ   NOT NULL,
    action           VARCHAR(16)   NOT NULL,
    settings_schema  VARCHAR(16),
    detector         VARCHAR(1024) COLLATE "C" NOT NULL,
    detector_version VARCHAR(64),
    category         VARCHAR(128)  NOT NULL,
    severity         VARCHAR(16)   NOT NULL,
    confidence       REAL          NOT NULL,
    suggested_action VARCHAR(16)   NOT NULL,
    span_start       INTEGER,
    span_end         INTEGER,
    excerpt          VARCHAR(2048),
    excerpt_start    INTEGER,
    CONSTRAINT pk_evidences PRIMARY KEY (trace_id, ordinal),
    CONSTRAINT ck_evidences_ordinal CHECK (ordinal BETWEEN 0 AND 255),
    CONSTRAINT ck_evidences_content_kind
        CHECK (content_kind IN ('prompt', 'response', 'upload', 'mcp_tool_call', 'mcp_tool_result')),
    CONSTRAINT ck_evidences_action
        CHECK (action IN ('allow', 'audit', 'warn', 'redact', 'block')),
    CONSTRAINT ck_evidences_suggested_action
        CHECK (suggested_action IN ('allow', 'audit', 'warn', 'redact', 'block')),
    CONSTRAINT ck_evidences_severity
        CHECK (severity IN ('low', 'medium', 'high', 'critical')),
    CONSTRAINT ck_evidences_confidence CHECK (confidence BETWEEN 0 AND 1),
    CONSTRAINT ck_evidences_span
        CHECK ((span_start IS NULL AND span_end IS NULL)
            OR (span_start >= 0 AND span_end >= span_start)),
    CONSTRAINT ck_evidences_excerpt
        CHECK ((excerpt IS NULL AND excerpt_start IS NULL)
            OR (excerpt IS NOT NULL AND excerpt_start >= 0
                AND octet_length(excerpt) <= 8192)),
    CONSTRAINT ck_evidences_expiry CHECK (expires_at > created_at)
);

CREATE INDEX idx_evidences_tenant_created ON evidences (tenant_id, created_at);
CREATE INDEX idx_evidences_correlation    ON evidences (correlation_id) WHERE correlation_id IS NOT NULL;
CREATE INDEX idx_evidences_expires        ON evidences (expires_at);
"#;

/// `SQLite` has no column lengths, so the length limits are `CHECK`s:
/// characters for text columns, bytes for the excerpt.
pub const SQLITE: &str = r"
CREATE TABLE evidences (
    trace_id         TEXT    NOT NULL,
    ordinal          INTEGER NOT NULL,
    tenant_id        TEXT    NOT NULL,
    correlation_id   TEXT    CHECK (length(correlation_id) <= 255),
    connector_id     TEXT    NOT NULL CHECK (length(connector_id) <= 255),
    content_kind     TEXT    NOT NULL,
    created_at       TEXT    NOT NULL,
    expires_at       TEXT    NOT NULL,
    action           TEXT    NOT NULL,
    settings_schema  TEXT    CHECK (length(settings_schema) <= 16),
    detector         TEXT    COLLATE BINARY NOT NULL CHECK (length(detector) <= 1024),
    detector_version TEXT    CHECK (length(detector_version) <= 64),
    category         TEXT    NOT NULL CHECK (length(category) <= 128),
    severity         TEXT    NOT NULL,
    confidence       REAL    NOT NULL,
    suggested_action TEXT    NOT NULL,
    span_start       INTEGER,
    span_end         INTEGER,
    excerpt          TEXT,
    excerpt_start    INTEGER,
    CONSTRAINT pk_evidences PRIMARY KEY (trace_id, ordinal),
    CONSTRAINT ck_evidences_ordinal CHECK (ordinal BETWEEN 0 AND 255),
    CONSTRAINT ck_evidences_content_kind
        CHECK (content_kind IN ('prompt', 'response', 'upload', 'mcp_tool_call', 'mcp_tool_result')),
    CONSTRAINT ck_evidences_action
        CHECK (action IN ('allow', 'audit', 'warn', 'redact', 'block')),
    CONSTRAINT ck_evidences_suggested_action
        CHECK (suggested_action IN ('allow', 'audit', 'warn', 'redact', 'block')),
    CONSTRAINT ck_evidences_severity
        CHECK (severity IN ('low', 'medium', 'high', 'critical')),
    CONSTRAINT ck_evidences_confidence CHECK (confidence BETWEEN 0 AND 1),
    CONSTRAINT ck_evidences_span
        CHECK ((span_start IS NULL AND span_end IS NULL)
            OR (span_start >= 0 AND span_end >= span_start)),
    CONSTRAINT ck_evidences_excerpt
        CHECK ((excerpt IS NULL AND excerpt_start IS NULL)
            OR (excerpt IS NOT NULL AND excerpt_start >= 0
                AND length(excerpt) <= 2048
                AND length(CAST(excerpt AS BLOB)) <= 8192)),
    CONSTRAINT ck_evidences_expiry CHECK (expires_at > created_at)
);

CREATE INDEX idx_evidences_tenant_created ON evidences (tenant_id, created_at);
CREATE INDEX idx_evidences_correlation    ON evidences (correlation_id) WHERE correlation_id IS NOT NULL;
CREATE INDEX idx_evidences_expires        ON evidences (expires_at);
";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sql = match manager.get_database_backend() {
            sea_orm::DatabaseBackend::Postgres => POSTGRES,
            sea_orm::DatabaseBackend::Sqlite => SQLITE,
            other => {
                return Err(DbErr::Migration(format!(
                    "prompt-scanner supports PostgreSQL and SQLite, not {other:?}"
                )));
            }
        };
        manager.get_connection().execute_unprepared(sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE evidences;")
            .await?;
        Ok(())
    }
}
