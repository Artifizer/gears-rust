---
description: "Product requirements for the prompt-scanner gear: tenant-aware inspection of GenAI content that returns one verdict per scan from a pluggable set of scanner backends."
---

# PRD — prompt-scanner

<!-- toc -->

- [1. Overview](#1-overview)
  - [1.1 Purpose](#11-purpose)
  - [1.2 Background / Problem Statement](#12-background--problem-statement)
  - [1.3 Goals (Business Outcomes)](#13-goals-business-outcomes)
  - [1.4 Glossary](#14-glossary)
- [2. Actors](#2-actors)
  - [2.1 Human Actors](#21-human-actors)
  - [2.2 System Actors](#22-system-actors)
- [3. Operational Concept & Environment](#3-operational-concept--environment)
  - [3.1 Gear-Specific Environment Constraints](#31-gear-specific-environment-constraints)
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Inspection](#51-inspection)
  - [5.2 Detection and Verdict](#52-detection-and-verdict)
  - [5.3 Policy](#53-policy)
  - [5.4 Evidence](#54-evidence)
  - [5.5 Extensibility](#55-extensibility)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Gear-Specific NFRs](#61-gear-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Scan a Prompt Before It Reaches a Model](#scan-a-prompt-before-it-reaches-a-model)
  - [Scan an Uploaded File](#scan-an-uploaded-file)
  - [Investigate a Flagged Request](#investigate-a-flagged-request)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

`prompt-scanner` is a tenant-aware inspection and verdict service for GenAI traffic. A
connector (an AI gateway hook, an application, any service calling its REST API or its
in-process client) submits a piece of content — a prompt, a model response, an uploaded
file, or an MCP tool call or result — and gets back one verdict: `allow`, `audit`, `warn`,
`redact` or `block`, with the findings that produced it.

Detection itself lives in scanner backend plugins. `prompt-scanner` owns what must not be
repeated in every detector: authorization, the caller's tenant policy, the concurrent
fan-out across the backends that policy enables, per-backend timeouts, verdict
aggregation, redaction, text extraction from attachments, reporting of backends that did
not contribute, and the stored findings of flagged scans.

### 1.2 Background / Problem Statement

People send sensitive material to AI tools: personal data, credentials, financial
identifiers, internal documents. Each place that captures such traffic — a gateway, an
endpoint agent, a first-party chat feature — would otherwise need its own detectors, its
own policy and its own audit trail, and they would drift apart.

The scanner does not proxy or forward LLM traffic. Connectors capture traffic, call the
scanner, and apply the returned verdict. `prompt-scanner` is therefore a service that
decides, not an interception product; it never terminates TLS, forwards traffic or holds
provider credentials.

Detection quality and cost vary widely between techniques (deterministic patterns versus
ML models), so detectors must be pluggable and composable per tenant, and a slow or failed
detector must never make a scan look clean.

### 1.3 Goals (Business Outcomes)

- One inspection API that every connector uses, so policy and findings are consistent no
  matter where content was captured.
- New detectors are added as plugins, with no change to `prompt-scanner` or to its
  connectors.
- Each tenant controls which detectors run and what happens on a hit, from one policy.
- A connector can always tell a fully scanned verdict from one with degraded coverage.
- Every flagged scan can be traced afterwards by its trace id or by the connector's own
  correlation id, for as long as its retention allows.

### 1.4 Glossary

| Term | Definition |
|------|------------|
| Scan | One inspection of one piece of content, identified by a `trace_id`. |
| Verdict | The single action returned for a scan: `allow`, `audit`, `warn`, `redact` or `block`, ordered by severity in that order. |
| Finding | One detection by one backend: category, severity, confidence, optional span and the backend's suggested action, stamped with the backend's identity and version. |
| Scanner backend | A plugin gear that implements `ScannerBackendClient` and registers a `ScannerBackendSpecV1` instance. |
| Unscored backend | An enabled, non-required backend that did not contribute to a scan (timeout, unavailable, error or deferred), listed on the verdict. |
| Tenant policy | Per-tenant settings: enabled and required backends, per-category thresholds and action overrides, default action, scanned content kinds, retention and evidence settings. |
| Policy provider | The single authoritative source of tenant policy; a plugin, or the built-in read-only provider that serves the fallback policy from configuration. |
| Fail mode | What the gear does when it cannot resolve a tenant's policy: `closed` (refuse, the default) or `open` (warn and fall back). |
| Flagged scan | A scan with at least one finding. Only flagged scans are stored. |
| Evidence | The stored findings of a flagged scan, optionally with a short excerpt around each finding when the tenant policy enables it. |
| Defer | A backend's answer that an input is too expensive to score inline; it is called again in the background and its findings join the same scan. |
| Connector | The caller that submitted the content, named by `connector_id`. |

## 2. Actors

### 2.1 Human Actors

#### Tenant Security Administrator

**ID**: `cpt-cf-prompt-scanner-actor-security-admin`

- **Role**: Reads and updates the tenant's scanning policy, and investigates or removes the stored findings of flagged scans.
- **Needs**: Control over which backends run and what each category of finding does; a way to find a flagged scan from a connector's request id; a way to remove stored evidence before it expires.

#### Platform Operator

**ID**: `cpt-cf-prompt-scanner-actor-operator`

- **Role**: Deploys `prompt-scanner` and its backends, and sets the fail mode, fallback policy, timeouts, queue sizes and retention ceilings in configuration.
- **Needs**: Safe defaults, visible backend availability, bounded resource use.

### 2.2 System Actors

#### Connector

**ID**: `cpt-cf-prompt-scanner-actor-connector`

- **Role**: Any service that submits content and enforces the verdict, through the REST API or the in-process `PromptScannerClientV1` — for example a gateway hook plugin (planned for `llm-gateway`).

#### Scanner Backend

**ID**: `cpt-cf-prompt-scanner-actor-scanner-backend`

- **Role**: A detector plugin (`regex-scanner`, `onnx-scanner`, `always-allow-plugin`, or a third-party one) that returns detections for a text input, defers, or fails.

#### Policy Provider

**ID**: `cpt-cf-prompt-scanner-actor-policy-provider`

- **Role**: The plugin that owns tenant policy storage and decisioning, when one is registered; for example an adapter to an external policy service.

#### File Parser

**ID**: `cpt-cf-prompt-scanner-actor-file-parser`

- **Role**: The platform `file-parser` gear, which turns an uploaded file into text before backends run.

## 3. Operational Concept & Environment

Project-wide runtime, architecture and lifecycle rules are in
[`docs/ARCHITECTURE_MANIFEST.md`](../../../docs/ARCHITECTURE_MANIFEST.md) and the
[`guidelines/`](../../../guidelines/). This gear has no parent PRD.

### 3.1 Gear-Specific Environment Constraints

- Detectors are separate plugin gears discovered at runtime through `types-registry`; a
  deployment chooses its detector set by which plugins it builds in and enables.
- Text extraction from uploads depends on the `file-parser` gear being present.
- Asynchronous scans and deferred backend calls run on a bounded in-process job queue;
  queued work does not survive a process restart.

## 4. Scope

### 4.1 In Scope

- Synchronous and asynchronous scanning of prompts, responses, uploads, and MCP tool calls and results, with a readable job status for asynchronous scans.
- Concurrent fan-out to every backend the tenant's policy enables, with per-backend timeouts, and reporting of backends that did not contribute.
- Verdict aggregation driven by tenant policy, and span-based redaction.
- Text extraction from uploaded files before scanning.
- Pluggable tenant policy source with a configuration-driven fallback and a fail-closed default.
- Authorization of every operation, confined to the caller's tenant.
- Storage of flagged scans' findings with retention, lookup by trace id or correlation id, and manual removal.
- Listing registered backends and whether each can be called.

### 4.2 Out of Scope

- Capturing, forwarding or proxying LLM traffic, terminating TLS, or holding provider credentials. Connectors do this.
- Enforcing the verdict. The connector applies it.
- Detection logic itself; it lives in backend plugins.
- Storing tenant policies. They belong to the policy provider.
- Listing or searching stored scans beyond trace id and correlation id lookups.
- Storing clean scans or the scanned payload.
- A durable job queue.

## 5. Functional Requirements

### 5.1 Inspection

#### Synchronous Scan

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-sync-scan`

The system **MUST** accept a piece of content of kind `prompt`, `response`, `upload`, `mcp_tool_call` or `mcp_tool_result` from a connector and return one verdict with the findings that produced it and a `trace_id`.

- **Rationale**: One inspection call per piece of content is what every connector needs to decide whether to let it through.
- **Actors**: `cpt-cf-prompt-scanner-actor-connector`

#### Caller's Tenant and Authorization

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-caller-tenant`

The system **MUST** scan for the caller's own tenant, taken from its security context, and **MUST** refuse a caller without a tenant or not authorized to scan for that tenant. A scan request names no tenant. Every other operation — job status, evidence lookup and removal, policy read and update, backend listing — **MUST** likewise be authorized and confined to the caller's tenant.

- **Rationale**: Tenant isolation; a connector cannot select another tenant's policy or read or write another tenant's evidence.
- **Actors**: `cpt-cf-prompt-scanner-actor-connector`, `cpt-cf-prompt-scanner-actor-security-admin`

#### Attachment Scanning

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-attachment-scan`

The system **MUST** accept an uploaded file, extract its text through `file-parser` within a configured time, and scan the extracted text; a file whose text cannot be extracted in time, or at all, **MUST** fail the scan rather than pass as clean.

- **Rationale**: Files are a main path for sensitive data into AI tools, and connectors should send raw bytes once instead of each integrating a parser.
- **Actors**: `cpt-cf-prompt-scanner-actor-connector`, `cpt-cf-prompt-scanner-actor-file-parser`

#### Asynchronous and Deferred Scans

- [ ] `p2` - **ID**: `cpt-cf-prompt-scanner-fr-deferred-scan`

The system **MUST** let a connector queue a scan and receive a job handle instead of a verdict, and read the job's status (queued, running, completed with its `trace_id`, or failed with a stable reason code) for a configured time after it finishes. A full queue **MUST** refuse new jobs rather than grow. The system **MUST** let a backend defer an input it cannot score inline, call it again in the background marked as the deferred run, and add its findings to the same scan under the same `trace_id`.

- **Rationale**: Large uploads and expensive detectors must not hold the synchronous path, and their findings must still reach the scan's record.
- **Actors**: `cpt-cf-prompt-scanner-actor-connector`, `cpt-cf-prompt-scanner-actor-scanner-backend`

### 5.2 Detection and Verdict

#### Backend Fan-out

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-backend-fanout`

The system **MUST** run every scanner backend enabled in the tenant's policy, concurrently, each under a configurable timeout, and **MUST NOT** run backends the policy does not enable.

- **Rationale**: Detectors are independent voters; the slowest one should bound latency, not the sum of all.
- **Actors**: `cpt-cf-prompt-scanner-actor-scanner-backend`

#### No Silent Clean

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-no-silent-clean`

The system **MUST** fail a scan when a backend the tenant's policy marks as required errors, times out or cannot be resolved. A non-required backend that errors, times out, is unavailable or deferred **MUST** be listed on the synchronous verdict as unscored, with a stable reason, logged and counted; it **MUST NOT** change the verdict action. A backend confidence that is not a number in `[0, 1]` **MUST** count as certain.

- **Rationale**: A scan that skipped a detector must never be indistinguishable from a clean scan.
- **Actors**: `cpt-cf-prompt-scanner-actor-connector`, `cpt-cf-prompt-scanner-actor-scanner-backend`

#### Verdict Aggregation

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-verdict-aggregation`

The system **MUST** derive the verdict from the tenant's policy: drop findings below their category's confidence threshold, replace a finding's suggested action with the most specific category override (keys match whole dot-separated segments), take the most severe remaining action above `allow`, and use the policy's default action when none remains. Content of a kind the policy does not govern **MUST** be allowed without running backends.

- **Rationale**: The tenant, not the detector, decides what a category of finding means for its users, and a backend reporting `allow` cannot lower a stricter default.
- **Actors**: `cpt-cf-prompt-scanner-actor-security-admin`

#### Redaction

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-redaction`

When the verdict of a synchronous scan is `redact`, the system **MUST** return the content with the spans of redact-suggesting findings replaced by a placeholder, merging overlapping spans, and replacing the whole text when such a finding has no span or a malformed one.

- **Rationale**: Lets a connector forward a safe version of the content instead of blocking the user.
- **Actors**: `cpt-cf-prompt-scanner-actor-connector`

### 5.3 Policy

#### Pluggable Policy Source

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-policy-provider`

The system **MUST** obtain tenant policy from exactly one authoritative policy provider — the highest-priority registered provider plugin, or, when none is registered, a built-in read-only provider that serves the fallback policy from the gear's configuration — and **MUST** expose reading and updating the caller's tenant policy through that provider, refusing updates a read-only provider cannot take. When the policy cannot be resolved (no provider and no fallback, a malformed or client-less provider, an invalid served policy), the system **MUST** by default refuse the request; an operator may configure it to fall back instead.

- **Rationale**: Policy originates in a policy service outside this gear, and scanning with a guessed policy is a silent failure.
- **Actors**: `cpt-cf-prompt-scanner-actor-security-admin`, `cpt-cf-prompt-scanner-actor-policy-provider`, `cpt-cf-prompt-scanner-actor-operator`

### 5.4 Evidence

#### Store Flagged Scans

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-evidence-storage`

The system **MUST** store the findings of every flagged scan with an expiry time and **MUST NOT** store clean scans or the scanned payload. A short excerpt around a finding **MUST** be stored only when the tenant's evidence policy enables it.

- **Rationale**: An audit trail for what was detected, without turning the scanner into a copy of everything users typed.
- **Actors**: `cpt-cf-prompt-scanner-actor-security-admin`

#### Evidence Lookup

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-evidence-lookup`

The system **MUST** return a stored scan of the caller's tenant by its `trace_id`, and the stored scans of the caller's tenant that carry a given correlation id, newest first and bounded in size. It **MUST NOT** offer unrestricted listing of stored scans.

- **Rationale**: Investigations start from a known request; open-ended browsing of findings is not a product need and widens exposure.
- **Actors**: `cpt-cf-prompt-scanner-actor-security-admin`, `cpt-cf-prompt-scanner-actor-connector`

#### Evidence Removal

- [ ] `p2` - **ID**: `cpt-cf-prompt-scanner-fr-evidence-delete`

The system **MUST** let an authorized caller remove a stored scan's findings and evidence of its own tenant before they expire.

- **Rationale**: Data-subject and incident-response requests cannot wait for retention.
- **Actors**: `cpt-cf-prompt-scanner-actor-security-admin`

### 5.5 Extensibility

#### Backend Extension

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-fr-backend-extension`

The system **MUST** include any gear that registers a scanner backend instance in the fan-out for tenants whose policy enables it, with no change to `prompt-scanner` or to its connectors.

- **Rationale**: New detectors (prompt injection, harmful content, vendor-specific secrets) are added as plugins.
- **Actors**: `cpt-cf-prompt-scanner-actor-scanner-backend`

#### Backend Listing

- [ ] `p2` - **ID**: `cpt-cf-prompt-scanner-fr-backend-listing`

The system **MUST** list, to an authorized caller, the registered scanner backend instances and whether each one can currently be called.

- **Rationale**: Operators need to see a registered-but-unavailable backend before a tenant policy depends on it.
- **Actors**: `cpt-cf-prompt-scanner-actor-operator`

## 6. Non-Functional Requirements

Project-wide baselines are in [`docs/ARCHITECTURE_MANIFEST.md`](../../../docs/ARCHITECTURE_MANIFEST.md)
and [`guidelines/`](../../../guidelines/). Only gear-specific NFRs follow.

### 6.1 Gear-Specific NFRs

#### No Raw Content in Telemetry

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-nfr-no-raw-content`

The system **MUST NOT** write scanned payloads, attachment bytes or metadata values to logs, metrics, traces, health output or error responses.

- **Threshold**: Zero occurrences of payload text in any telemetry channel; debug formatting of request, input and verdict types prints lengths only; error responses carry no internal error text.
- **Rationale**: The content under inspection is by definition sensitive.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Bounded Backend Latency

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-nfr-backend-timeout`

The system **MUST** bound each backend call by a configurable timeout, so that a synchronous scan's backend phase never waits longer than that timeout.

- **Threshold**: Default 5 s per backend (configurable up to 5 min); backends run concurrently, so the backend phase is bounded by one timeout, not their sum. Deferred calls and attachment extraction have their own bounds (defaults 60 s and 30 s).
- **Rationale**: Connectors sit on a user's request path.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Tenant Isolation of Evidence

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-nfr-tenant-isolation`

The system **MUST NOT** return, or reveal the existence of, another tenant's stored scans or jobs: an unknown, expired, clean or foreign scan **MUST** produce the same not-found answer.

- **Threshold**: Zero cross-tenant reads in tests; responses for the four cases are identical.
- **Rationale**: Findings describe sensitive content of a tenant's users.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Bounded Evidence Retention

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-nfr-evidence-retention`

The system **MUST** delete stored findings after their expiry, with per-tenant retention clamped to operator-configured ceilings.

- **Threshold**: Default ceilings 90 days for a finding and 30 days for a finding with an excerpt; expired rows removed by a periodic job (default every 5 minutes).
- **Rationale**: Stored findings are personal data in many deployments.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Bounded Queues

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-nfr-bounded-queues`

The system **MUST** bound every in-memory queue — asynchronous jobs, evidence writes, job statuses — and **MUST** shed load visibly when one is full rather than grow without limit.

- **Threshold**: Defaults of 1,024 queued jobs and 1,024 pending evidence writes; job statuses forgotten 1 h after the job finishes; a full job queue answers `503`, a full write queue drops and counts the scan's rows.
- **Rationale**: Content arrives from untrusted callers; unbounded queues turn a burst into memory exhaustion.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

#### Verdict Not Held by Storage

- [ ] `p2` - **ID**: `cpt-cf-prompt-scanner-nfr-evidence-write-isolation`

The system **MUST** return the verdict without waiting for evidence storage, and a failed evidence write **MUST NOT** fail the scan; it **MUST** be counted in metrics.

- **Threshold**: Scan latency independent of database write latency; every lost write counted.
- **Rationale**: The audit trail must not slow down or break the user's request.
- **Architecture Allocation**: See DESIGN.md § NFR Allocation

### 6.2 NFR Exclusions

- Durable job queue: queued asynchronous and deferred work is in memory and is lost on restart; accepted for this version and recorded as a risk.
- Detection accuracy targets: owned by each backend plugin, not by this gear.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### REST API

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-interface-rest-api`

- **Type**: REST API under `/prompt-scanner/v1`
- **Stability**: unstable
- **Description**: Submit scans (sync and async) and read job status; fetch, find by correlation id and remove stored scans; read and update the caller's tenant policy; list backends.
- **Breaking Change Policy**: New major path version.

#### In-process Client

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-interface-client-v1`

- **Type**: Rust trait `PromptScannerClientV1` in `cf-gears-prompt-scanner-sdk`, resolved through `ClientHub`
- **Stability**: unstable
- **Description**: The same operations as the REST API, for connectors in the same process.
- **Breaking Change Policy**: A new trait version (`V2`) for breaking changes.

#### Scanner Backend Plugin API

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-interface-scanner-backend`

- **Type**: Rust trait `ScannerBackendClient` with GTS type `ScannerBackendSpecV1`
- **Stability**: unstable
- **Description**: What a detector implements to join the fan-out.
- **Breaking Change Policy**: A new GTS type version for breaking changes.

#### Policy Provider Plugin API

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-interface-policy-provider`

- **Type**: Rust trait `PolicyProvider` with GTS type `PolicyProviderSpecV1`
- **Stability**: unstable
- **Description**: What a policy source implements to become the authoritative tenant policy provider.
- **Breaking Change Policy**: A new GTS type version for breaking changes.

### 7.2 External Integration Contracts

#### Plugin Registration

- [ ] `p2` - **ID**: `cpt-cf-prompt-scanner-contract-plugin-registration`

- **Direction**: required from client (plugin gears)
- **Protocol/Format**: A `PluginV1<Spec>` instance registered in `types-registry` plus a scoped `ClientHub` entry keyed by the instance's GTS id.
- **Compatibility**: Instance ids are stable; a new spec version is a new GTS type.

## 8. Use Cases

### Scan a Prompt Before It Reaches a Model

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-usecase-scan-prompt`

**Actor**: `cpt-cf-prompt-scanner-actor-connector`

**Preconditions**:
- The connector is authenticated and authorized to scan for its tenant.
- The tenant's policy can be resolved and enables at least one backend.

**Main Flow**:
1. The connector submits the prompt with `content_kind = prompt` and an optional correlation id.
2. The system resolves the tenant's policy and runs the enabled backends concurrently.
3. The system aggregates findings into a verdict and, for `redact`, builds the redacted text.
4. The system returns the verdict, listing any unscored backends; a flagged scan's findings are stored afterwards.
5. The connector forwards, redacts, warns or blocks according to the verdict.

**Postconditions**:
- A flagged scan can be looked up by `trace_id` or correlation id until it expires.

**Alternative Flows**:
- **A required backend fails**: the scan fails; the connector applies its own failure handling.
- **An optional backend fails or defers**: the verdict lists it as unscored; deferred findings are added later under the same `trace_id`.
- **The policy cannot be resolved**: the request is refused unless the operator configured fail-open.

### Scan an Uploaded File

- [ ] `p1` - **ID**: `cpt-cf-prompt-scanner-usecase-scan-upload`

**Actor**: `cpt-cf-prompt-scanner-actor-connector`

**Preconditions**:
- `file-parser` is available.

**Main Flow**:
1. The connector submits the file's bytes with `content_kind = upload`, synchronously or as an asynchronous job.
2. The system extracts the text through `file-parser`.
3. The scan continues as for a prompt; an asynchronous job reports `completed` with the `trace_id`.

**Postconditions**:
- The verdict covers the file's extracted text.

**Alternative Flows**:
- **Text cannot be extracted, or not in time**: the scan fails with an attachment-parsing or timeout error.

### Investigate a Flagged Request

- [ ] `p2` - **ID**: `cpt-cf-prompt-scanner-usecase-investigate`

**Actor**: `cpt-cf-prompt-scanner-actor-security-admin`

**Preconditions**:
- The administrator is authorized and has the connector's request id, or the scan's `trace_id`.

**Main Flow**:
1. The administrator looks up the stored scans by correlation id or `trace_id`.
2. The system returns the stored findings of the caller's tenant, newest first.
3. If required, the administrator removes the stored evidence.

**Postconditions**:
- Removed evidence is no longer returned.

**Alternative Flows**:
- **Nothing stored**: the scan was clean, unknown, expired or of another tenant; the same not-found answer is returned.

## 9. Acceptance Criteria

- [ ] A prompt containing an e-mail address and a Luhn-valid card number, scanned with `regex-scanner` enabled, returns `block`; the same prompt without the card number returns `redact` and the text with the address replaced.
- [ ] A tenant policy that marks a backend required makes the scan fail when that backend is unavailable; an optional unavailable backend is listed as unscored and the verdict is unchanged.
- [ ] With no policy provider and no fallback policy, a scan is refused under the default fail mode.
- [ ] A clean scan stores nothing; a flagged scan is returned by `trace_id` and by correlation id for its own tenant and not for another tenant.
- [ ] An asynchronous scan's job status moves to `completed` with its `trace_id`; a full job queue refuses new jobs.
- [ ] Removing a scan's evidence makes later lookups return not-found.
- [ ] A new backend plugin, once registered and enabled in a tenant policy, contributes findings without any change to `prompt-scanner`.

## 10. Dependencies

| Dependency | Description | Criticality |
|------------|-------------|-------------|
| `types-registry` | Discovery of scanner backend and policy provider plugin instances | p1 |
| `authz-resolver` | Authorization of every operation | p1 |
| `file-parser` | Text extraction from uploaded files | p1 |
| Database (PostgreSQL or SQLite) | Storage of flagged scans' findings | p1 |
| Scanner backend plugins | Detection; at least one must be enabled for a scan to find anything | p1 |

## 11. Assumptions

- Connectors enforce the verdict they receive and check `unscored_backends` when they need full coverage; `prompt-scanner` cannot check either.
- Backends report byte-offset spans into the text they were given, so redaction is accurate.
- A deployment that needs per-tenant policy registers a policy provider plugin; without one, every tenant gets the same fallback policy.

## 12. Risks

| Risk | Impact | Mitigation |
|------|--------|------------|
| Queued asynchronous or deferred work is lost on restart | Some deferred findings are never stored; a queued job's status disappears | Known limitation of the in-memory queue; the synchronous verdict is unaffected and lists deferred backends as unscored |
| An empty `enabled_backends` list means nothing is detected | Content passes as clean | Explicit by design; the fallback policy in configuration names the backends to run |
| A connector ignores `unscored_backends` | Degraded coverage is treated as clean | Tenants mark backends `required` where coverage is mandatory |
| A backend reports wrong spans | Redaction misses or over-redacts | A finding without a valid span redacts the whole text |
| A full write queue or failed evidence write loses that scan's findings | Gap in the audit trail | Counted in metrics so the loss is visible |

## 13. Open Questions

- Whether stored findings need per-tenant redaction rules beyond retention for regulated, customer-hosted deployments.
- How a connector is notified when a deferred backend completes (polling, webhook or event).
- Whether long inputs should be chunked so size-limited backends can scan them instead of failing.

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
