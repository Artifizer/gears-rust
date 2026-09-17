# prompt-scanner

Tenant-aware inspection and verdict gear. Content goes in — prompts, model responses,
file uploads — and a single verdict comes out: `Allow`, `Audit`, `Warn`, `Redact` or
`Block`, with the findings that produced it recorded for audit.

Detection itself lives in plugins. `prompt-scanner` owns the parts that must not be
duplicated per detector: tenant policy, the concurrent fan-out across enabled backends,
per-backend timeouts, verdict aggregation, redaction, attachment text extraction, and the
stored findings of flagged scans.

See [docs/DESIGN.md](../docs/DESIGN.md) for the architecture (§3.3 lists every REST
route) and [docs/PRD.md](../docs/PRD.md) for the requirements. The OpenAPI document the
gear registers is the reference for request and response shapes.

## What it does

- **Inspection API** — `POST /scans` synchronously, or `POST /scans/async` (202) for large
  uploads; poll `GET /scans/async/{job_id}`. A backend may also defer slow work (no
  shipped backend does); the worker calls it again marked as a deferred run, and its
  findings join the same `trace_id` later. A full job queue answers 503.
- **Stored findings** — only scans with findings are stored, until their retention expires.
  Look one up with `GET /scans/{trace_id}` or `GET /scans?correlation_id=…`, or remove it
  early with `DELETE /scans/{trace_id}`. Stored scans cannot be listed.
- **Scanner backends as plugins** — any gear implementing `ScannerBackendClient` and
  registering a `ScannerBackendSpecV1` instance joins the fan-out. Shipped backends:
  `regex-scanner` (deterministic) and `onnx-scanner` (ML).
- **Per-tenant policy** — which backends run, per-category confidence thresholds,
  category→action overrides, and a `default_action` fallback. Supplied by any gear
  registering a `PolicyProviderSpecV1` instance, or, without one, by the built-in read-only
  provider serving the config's `fallback_policy`. The gear stores no tenant policies.
- **Verdict aggregation** — findings below a category's threshold are dropped; the
  aggregate action is the maximum of what remains above `Allow`, else the tenant's
  `default_action` (an `Allow` finding never lowers it). One backend voting `Block` wins.
- **Degrades loudly** — an optional backend that errors, times out, is unavailable or
  defers is listed in the verdict's `unscored_backends` (reason `error`, `timeout`,
  `unavailable` or `deferred`), logged and counted, and never changes the action; a
  *required* backend that cannot be resolved, errors (503) or times out (504) fails the
  scan rather than passing content as clean.
- **Fails closed** — with `fail_mode: closed` (the default) an unresolvable policy (no
  provider and no `fallback_policy`, a malformed provider instance, a provider without a
  client, an invalid served policy) fails the request with 503 instead of scanning nothing.
- **Authorized throughout** — every call goes through the PDP: scan records
  (`create`/`get`/`delete`), the tenant policy (`get`/`update`) and the backend list
  (`list`). A tenantless token is refused.

## Configuration

```yaml
prompt-scanner:
  database:
    server: sqlite_users
    file: "prompt_scanner.db"
  config:
    fail_mode: closed              # or open: warn and use the fallback/permissive policy
    backend_timeout: 5s            # per backend, synchronous path (alias: backend_timeout_ms)
    deferred_backend_timeout: 60s  # a deferred backend's background call
    attachment_parse_timeout: 30s  # file-parser text extraction
    job_poll_interval: 200ms       # async worker tick (alias: job_poll_interval_ms)
    jobs_per_tick: 32
    job_queue_capacity: 1024       # full queue: scan_async answers 503
    job_status_ttl: 1h             # finished job statuses are forgotten after this
    fallback_policy: { ... }       # the policy when no provider plugin is registered
    evidence:
      max_retention: 90d
      max_evidence_retention: 30d  # at most max_retention
      evidence_context_chars: 256  # at most 2048
      max_findings_stored: 256
      retention_interval: 5m
      retention_batch_size: 10000
      write_queue_capacity: 1024   # full queue: the scan's rows are dropped and counted
      write_timeout: 10s
```

Every value is checked at start-up; a zero, out-of-range or out-of-order value stops the
gear with an error naming the key.

A backend must be enabled in the tenant's policy before it runs; an empty
`enabled_backends` means no detection, by design rather than by accident.
