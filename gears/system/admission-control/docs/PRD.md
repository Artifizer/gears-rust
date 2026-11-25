---
description: "Product requirements for Admission Control: the platform governance gate that runs ordered Governance Stages behind one decision API."
---

<!-- cpt:
version: 2.0.0
status: draft
module: admission-control
system: cf
-->

# PRD — Admission Control

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
- [4. Scope](#4-scope)
  - [4.1 In Scope](#41-in-scope)
  - [4.2 Out of Scope](#42-out-of-scope)
- [5. Functional Requirements](#5-functional-requirements)
  - [5.1 Governance Check](#51-governance-check)
  - [5.2 Stages and Declarations](#52-stages-and-declarations)
  - [5.3 Failure, Events and Configuration](#53-failure-events-and-configuration)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Module-Specific NFRs](#61-module-specific-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
  - [7.1 Public API Surface](#71-public-api-surface)
  - [7.2 External Integration Contracts](#72-external-integration-contracts)
- [8. Use Cases](#8-use-cases)
  - [Govern an Operation](#govern-an-operation)
  - [Add or Substitute a Stage](#add-or-substitute-a-stage)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Admission Control is the governance gate of the platform, as decided in [Operation Governance ADR 0001](../../../../docs/arch/operation-governance/ADR/0001-cpt-cf-operation-governance-adr-governance-gate.md). A Consuming Gear, through its Governance Enforcer, describes one attempt of a Governed Operation and gets one Governance Outcome. The gate validates the request, resolves which Governance Stages apply, runs them in a fixed order, and combines their results. Stages are plugins: Policy Engine provides the Guardrail Stage by default, and Approval Service provides the Approval Stage. The gate also merges what each stage reports into the Applicability Snapshot, so that operations no Policy governs need no call at all.

The first version of the gate (`AdmissionClientV1::admit` over one engine plugin) stays available until its callers migrate. It runs only the Guardrail Stage.

### 1.2 Background / Problem Statement

Without one gate, every Consuming Gear would order the checks, combine their results, and link their audit records on its own, and enforcement would drift between gears. Adding a new kind of governance would change every Consuming Gear. One gate gives one contract, one order, one fail-closed rule, and one refusal trail, while each kind of Policy stays in the stage that owns it.

### 1.3 Goals (Business Outcomes)

- One governance decision contract for every Consuming Gear, in every ToolKit deployment profile, with identity that the caller cannot spoof.
- Fail closed: a check that cannot be decided is refused in 100% of stage-absent, stage-failure, and timeout cases.
- New Governance Stages, including external decision engines, plug in without changes to Consuming Gears.
- Every refusal is published as an event, best effort: publication never delays or changes an outcome.

### 1.4 Glossary

Shared terms (Governed Operation, Facts, Governance Check, Governance Stage, Guardrail Stage, Approval Stage, Governance Profile, Governance Enforcer, Applicability Snapshot, Preview, Governance Outcome, Refusal Reason, Correlation ID, Trial Mode) are defined in the [Operation Governance glossary](../../../../docs/arch/operation-governance/PRD.md#14-glossary). Terms specific to this gear:

| Term | Definition |
|---|---|
| Stage result | What one Governance Stage returns for one check: permit, deny, approval required, approval pending, invalid, or a failure. |
| Refusal cause | Why the gate refused: stage policy, request too large, invalid request, or could not run. |
| Shadow denial | A denial by a Policy in Trial Mode; reported in events but never refuses. |
| Obligation | Data the Governance Enforcer must act on: the Approval Request reference, the unmet requirements, or the time to check again. |
| Missing-Facts answer | The names of Facts that the current Policies need and that a check did not carry. It is returned instead of an outcome, at most once per attempt. |
| Engine | The V1 name for the single Guardrail Stage behind `AdmissionClientV1`. |

## 2. Actors

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-cf-admission-control-actor-platform-operator`

- **Role**: Configures stage selection, Governance Profiles, timeouts, size bounds, and the snapshot age; a bad configuration fails startup.

#### Security Auditor

**ID**: `cpt-cf-admission-control-actor-security-auditor`

- **Role**: Reviews the published refusal and shadow events to see what was refused and why.

### 2.2 System Actors

#### Enforcing Gear

**ID**: `cpt-cf-admission-control-actor-enforcing-gear`

- **Role**: A Consuming Gear that calls the gate through its Governance Enforcer (`AdmissionClientV2`), or through `AdmissionClientV1` until it migrates, with its `SecurityContext`, and honours the outcome.

#### Governance Stage

**ID**: `cpt-cf-admission-control-actor-governance-stage`

- **Role**: A plugin implementing `GovernanceStagePluginClientV1`: Policy Engine (Guardrail Stage), Approval Service (Approval Stage), or a future or external stage.

#### Types Registry

**ID**: `cpt-cf-admission-control-actor-types-registry`

- **Role**: Lists Governance Stage instances, holds Governed Operation types, refuses breaking changes to them in place, and registers the event types.

#### Event Broker

**ID**: `cpt-cf-admission-control-actor-audit-sink`

- **Role**: Receives refusal events and Applicability Snapshot change notices. Optional for refusal events: when absent, they are dropped.

## 3. Operational Concept & Environment

The gate holds no persistent state. In the Embedded profile it runs in-process and is called through ClientHub. In the Self-Hosted and Kubernetes Native profiles it runs as a stateless, horizontally scaled unit together with the Guardrail Stage, and is called over the REST or gRPC projection of `AdmissionClientV2` ([Operation Governance DESIGN §3.8](../../../../docs/arch/operation-governance/DESIGN.md#38-deployment-topology)). Runtime, lifecycle, and integration patterns follow [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md), [guidelines/](../../../../guidelines/), [ToolKit out-of-process](../../../../docs/arch/toolkit-oop/PRD.md), and [ToolKit contract binding](../../../../docs/arch/toolkit-contract-binding/PRD.md).

## 4. Scope

### 4.1 In Scope

- One Governance Check returning one Governance Outcome, with a batch form for Previews.
- Governance Profile resolution, and discovery and ordered invocation of Governance Stages under timeouts.
- The Governed Operation base type, the Governance Enforcer, and the Fact provider contract, in the SDK.
- Validating Facts against the Governed Operation type, and asking once for missing Facts.
- Merging stage applicability into the signed Applicability Snapshot.
- Best-effort publication of refusal events and snapshot change notices.
- Startup validation of the configuration.
- `AdmissionClientV1::admit` until its callers migrate.

### 4.2 Out of Scope

- Policy storage, authoring, and evaluation: owned by the stages.
- Approval state: owned by Approval Service.
- Authorization and idempotency replay of completed operations: the Consuming Gear runs both before it calls the Governance Enforcer.
- Running, parking, or resuming the operation, and Plans: owned by the Consuming Gear and its Governance Enforcer.
- Modification of the caller's request.
- Tenant hierarchy resolution: the stages resolve tenancy.

## 5. Functional Requirements

> **Testing strategy**: All requirements verified via automated tests (unit, integration) targeting 90%+ code
> coverage unless otherwise specified.

### 5.1 Governance Check

#### Decision Interface

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-admission-interface`

The system **MUST** offer `AdmissionClientV2` with three operations: `check` (one Governed Operation, in enforce or Preview mode, returning an outcome or a missing-Facts answer), `check_batch` (one Governed Operation on a bounded list of resources, Preview only), and `applicability` (Applicability Snapshot entries of one tenant since a given version). It **MUST NOT** modify the request. Only calls that fail authentication or argument validation get an error instead of an outcome. Until its callers migrate, the system **MUST** also offer `AdmissionClientV1::admit`, which runs only the Guardrail Stage and returns admitted or refused.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

#### Identity From SecurityContext

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-request-authenticity`

The subject and its tenant **MUST** come only from the caller's `SecurityContext`; the request has no subject fields. An anonymous context **MUST** be rejected as unauthenticated, not refused. Calls on the platform plane **MUST** be rejected: Governed Operations are tenant-plane calls.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

#### Identifier Validation

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-identifier-validation`

The Governed Operation and the resource type **MUST** be GTS type IDs of at most 256 bytes. A malformed identifier **MUST** be rejected as an invalid argument that never echoes the value. A well-formed Governed Operation that is not a registered Governed Operation type **MUST** give **Invalid**.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

#### Request Size Bounds

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-request-bounds`

A request whose Fact count or serialized Fact size exceeds the configured bound, or whose Facts nest deeper than the fixed depth bound, or a batch longer than its configured bound, **MUST** be refused as too large before any stage runs.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-platform-operator`

#### Governance Profile Resolution

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-governance-profile`

The gate **MUST** resolve which stages apply from the Governed Operation type and the Product configuration, never from the request. If a stage in the Governance Profile is not deployed, the outcome **MUST** be **Unavailable**.

- **Actors**: `cpt-cf-admission-control-actor-platform-operator`, `cpt-cf-admission-control-actor-governance-stage`

#### Decision Order

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-decision-order`

The gate **MUST** authenticate the call, validate identifiers, check size bounds, validate the Facts, and resolve the Governance Profile before calling any stage. It **MUST** then call the applicable stages in their declared order, stages without side effects first and the stage with side effects (the Approval Stage) last, each at most once per check, and **MUST** stop at the first stage that does not permit. At most one stage with side effects **MUST** be configured. Every stage of one check **MUST** receive the same request and Correlation ID.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-governance-stage`

#### Governance Outcome and Refusal Cause

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-refusal-cause`

Every check **MUST** return exactly one Governance Outcome, mapped from stage results as defined in the [Operation Governance DESIGN](../../../../docs/arch/operation-governance/DESIGN.md#31-domain-model), with its Refusal Reason, refusal cause, what produced it (stage, Policy, and version, or Decisions), and its obligations. A denied operation and a failed check **MUST** stay distinguishable, and only a failure that may pass on retry **MUST** be reported as retryable: a defect in the gate or in the gate–stage contract is could-not-run `internal` and is not retryable. A stage result is mapped, never passed through.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

#### Preview

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-preview`

In Preview mode the gate **MUST** call every applicable stage in Preview mode, **MUST NOT** publish events, and **MUST** give the same outcome as enforce mode for the same inputs and state, except Approval required in place of opening a request. A stage that does not support Preview **MUST** make the Preview give **Unavailable**.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-governance-stage`

#### Correlation

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-correlation`

The gate **MUST** accept the caller's Correlation ID, mint one when it is absent, pass it to every stage, and put it in every outcome and event.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-security-auditor`

#### Fact Validation and Missing Facts

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-missing-facts`

The gate **MUST** give **Invalid** when a Fact is not declared by the Governed Operation type or does not match its schema, or when the call carries no calling gear identity or one that does not own the type. ToolKit attaches the calling gear's service identity to every call, including in-process ClientHub calls. It **MUST** compute the needed Facts from its own current Applicability Snapshot, not from the caller's version: the Facts that applicable Policies read, plus the Facts shown to Approvers when approval may be required. If a needed Fact is missing, the first check of the attempt **MUST** get a missing-Facts answer instead of an outcome. A needed Fact that is missing again, or that the caller reports it could not load, **MUST** give **Unavailable**.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-governance-stage`

### 5.2 Stages and Declarations

#### Stage Discovery

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-engine-selection`

The gate **MUST** discover stages through Types Registry. Each stage registration **MUST** declare its kind, order, whether it has side effects, and whether it supports Preview. For each stage kind, at most one instance **MUST** be in use, selected by `vendor` and optionally a pinned `instance_id`. A configured stage that cannot be resolved **MUST** fail startup. The Guardrail Stage is required; the Approval Stage and other stages are optional.

- **Actors**: `cpt-cf-admission-control-actor-governance-stage`, `cpt-cf-admission-control-actor-types-registry`

#### Governed Operation Base Type

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-governed-operation-type`

The SDK **MUST** own the abstract GTS base type `gts.cf.core.admission_control.governed_operation.v1~`. A Consuming Gear derives one type per operation, named by resource type and action, never by route. The top-level properties of its `facts` schema are its Facts. Its traits **MUST** give, per Fact, the Fact Tier and whether it is sensitive, shown to Approvers, or binding. Approval settings belong to the Approval Service opportunity that refers to the type. Renaming, retyping, or removing a Fact **MUST** need a new type version, which lists renamed Facts in a `replaces` trait. The gate **MUST** match Policies by the type ID without its version, pass the stages this family as the action, pass a renamed Fact under its old ID as well, and give **Unavailable** where an applicable Policy reads a Fact the current version removed or retyped. An Approval Service opportunity **MUST** refer to one Governed Operation family of the same owning gear and take its Facts from the family's current version.

- **Actors**: `cpt-cf-admission-control-actor-types-registry`, `cpt-cf-admission-control-actor-governance-stage`

#### Governance Enforcer

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-enforcer`

The SDK **MUST** provide the Governance Enforcer for Consuming Gears. It **MUST** cache Applicability Snapshot entries per tenant, ask the gear's Fact provider only for the Facts that are read, by their IDs in the gear's current type version (translating renamed Facts through `replaces`), one call per Fact Tier, make one check plus at most one more after a missing-Facts answer, and map the outcome to canonical errors. The SDK **MUST** generate a typed Fact API from each Governed Operation type, and **MUST** provide a conformance kit that fails a gear's tests when a governed route reaches its first side effect without a check for each governed operation it performs (a batch Preview counts as one). (p2) It **MUST** evaluate local CEL rules from the snapshot within a cost bound, and send a rule to the gate when its evaluator profile version is newer than the SDK's.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`

#### Applicability Snapshot

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-applicability`

The gate **MUST** merge what every stage reports, per Governed Operation and tenant scope, into a versioned, signed Applicability Snapshot: whether any Policy or a Default other than Allow applies, which Facts the applicable Policies read, and (p2) the local CEL rules. It **MUST** serve one tenant's changes since a given version and publish a change notice, with the tenant scope, when the version increases. A stage that cannot report **MUST** make every operation in its scope count as governed. Every Governed Operation whose Governance Profile includes a stage that is not deployed **MUST** count as governed for every tenant, so that its check gives **Unavailable** instead of being skipped.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-governance-stage`, `cpt-cf-admission-control-actor-audit-sink`

### 5.3 Failure, Events and Configuration

#### Fail Closed

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-fail-closed`

When an applicable stage is absent, cannot be resolved, is unavailable, times out, or fails, the check **MUST** be refused as could-not-run with the matching condition. The one exception is a stage that judges the Facts invalid: the check **MUST** then give **Invalid**, which is not retryable. Each stage call **MUST** run under its configured timeout.

- **Actors**: `cpt-cf-admission-control-actor-enforcing-gear`, `cpt-cf-admission-control-actor-governance-stage`

#### Refusal Events

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-refusal-events`

In enforce mode the gate **MUST** publish one refusal event per (operation, Policy) pair for every refusal and one per shadow denial (marked not enforced), never for Allowed outcomes or Approval pending. Events carry the Correlation ID, subjects, cause, stage, and Fact names (never values). Publication **MUST** be best effort: a full queue or an absent or failing broker drops the event and never delays or changes an outcome.

- **Actors**: `cpt-cf-admission-control-actor-security-auditor`, `cpt-cf-admission-control-actor-audit-sink`

#### Startup Configuration Validation

- [ ] `p1` - **ID**: `cpt-cf-admission-control-fr-configuration-validation`

Startup **MUST** fail on unknown configuration keys, a numeric setting of zero, or more than one stage with side effects.

- **Actors**: `cpt-cf-admission-control-actor-platform-operator`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide NFRs are defined in
> [docs/ARCHITECTURE_MANIFEST.md](../../../../docs/ARCHITECTURE_MANIFEST.md) and
> [guidelines/](../../../../guidelines/). Subsystem NFRs are in the
> [Operation Governance PRD](../../../../docs/arch/operation-governance/PRD.md#6-non-functional-requirements). Only gear-specific NFRs are listed here.

### 6.1 Module-Specific NFRs

#### Fail-Closed Determinism

- [ ] `p1` - **ID**: `cpt-cf-admission-control-nfr-fail-closed`

No failure of a stage, the event path, or the broker **MUST** ever produce an Allowed outcome that the stages did not give.

- **Threshold**: 0 Allowed outcomes on stage-absent, stage-failure, and timeout paths.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

#### Gate Overhead

- [ ] `p1` - **ID**: `cpt-cf-admission-control-nfr-overhead`

Validation, profile resolution, mapping, and outcome construction **MUST** add at most 5 ms p95 (10 ms p99) to a check, excluding stage calls, which are bounded by their timeouts.

- **Threshold**: 5 ms p95 / 10 ms p99 gate overhead; Guardrail Stage bound 100 ms by default.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

#### Stateless Scale-Out

- [ ] `p1` - **ID**: `cpt-cf-admission-control-nfr-scale-out`

The gate **MUST** keep no state between checks, so that adding instances increases throughput linearly.

- **Threshold**: Throughput scales linearly up to 8 instances in the load test.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

### 6.2 NFR Exclusions

- Availability SLOs: inherited from the platform; the gate is stateless.
- Event delivery guarantees: refusal events are best effort by design.

## 7. Public Library Interfaces

### 7.1 Public API Surface

#### Governance API

- [ ] `p1` - **ID**: `cpt-cf-admission-control-interface-governance-api`

`AdmissionClientV2` (remote-capable contract with REST and gRPC projections, registered in ClientHub): `check`, `check_batch`, and `applicability`. The request follows the OpenID AuthZEN shape: subject from the `SecurityContext`, action (Governed Operation type), resource (type, ID, tenant, Resource Version), context (Facts, and Facts the caller could not load), mode, snapshot version, round, and Correlation ID. `check` returns an outcome or a missing-Facts answer. A breaking change is a new parallel trait, per [contract versioning](../../../../docs/arch/toolkit-contract-binding/ADR/0007-cpt-cf-binding-adr-contract-versioning.md).

#### Admission Client

- [ ] `p1` - **ID**: `cpt-cf-admission-control-interface-admission-client`

`AdmissionClientV1::admit(ctx, &AdmissionRequest) -> Result<Verdict, AdmissionError>`, registered in ClientHub. Kept until its callers migrate to `AdmissionClientV2`, then deprecated with a sunset date.

#### Governance Stage Plugin Contract

- [ ] `p1` - **ID**: `cpt-cf-admission-control-interface-engine-plugin`

`GovernanceStagePluginClientV1` (remote-capable plugin contract with REST and gRPC projections), registered in ClientHub scoped by the stage's GTS instance: `evaluate` (enforce or Preview) returns a stage result or a failure; `applicability` returns the stage's applicability and the Facts each applicable Policy reads. It replaces `AdmissionEnginePluginClientV1`, which the Guardrail Stage keeps serving until `AdmissionClientV1` is retired.

### 7.2 External Integration Contracts

#### GTS Registration

- [ ] `p1` - **ID**: `cpt-cf-admission-control-contract-gts`

The gate owns the stage plugin spec `gts.cf.toolkit.plugins.plugin.v1~cf.core.admission_control.stage.v1~`, the V1 engine plugin spec `gts.cf.toolkit.plugins.plugin.v1~cf.core.admission_control.engine.v1~`, the Governed Operation base type `gts.cf.core.admission_control.governed_operation.v1~`, the error resource type `gts.cf.core.admission_control.admission.v1~`, and the refusal and snapshot-change event types.

#### Refusal Event Contract

- [ ] `p1` - **ID**: `cpt-cf-admission-control-contract-refusal-event`

One event type on the audit topic `gts.cf.core.events.topic.v1~cf.core.admission_control.audit.v1`, published under the gate's own identity; the payload is listed in DESIGN.md.

## 8. Use Cases

### Govern an Operation

- [ ] `p1` - **ID**: `cpt-cf-admission-control-usecase-gate-operation`

**Actor**: `cpt-cf-admission-control-actor-enforcing-gear`

**Preconditions**:
- The Governed Operation type is registered; the Applicability Snapshot says it is governed for the tenant.

**Main Flow**:
1. The Governance Enforcer calls `check` with its `SecurityContext`, the operation type, the resource, the Facts that are read, and the Correlation ID.
2. The gate validates the request and the Facts, and resolves the Governance Profile.
3. The gate calls the Guardrail Stage, then the Approval Stage, under their timeouts.
4. The gate publishes any refusal events and returns one Governance Outcome with its obligations.

**Postconditions**:
- The Consuming Gear runs the operation only on Allowed.

**Alternative Flows**:
- **A stage denies**: later stages are not called; the outcome is Blocked.
- **A stage fails or times out**: the outcome is Unavailable.
- **A needed Fact is missing**: the gate returns a missing-Facts answer; the Governance Enforcer sends those Facts once; a second miss gives Unavailable.

### Add or Substitute a Stage

- [ ] `p2` - **ID**: `cpt-cf-admission-control-usecase-substitute-engine`

**Actor**: `cpt-cf-admission-control-actor-platform-operator`

**Preconditions**:
- The new stage implements `GovernanceStagePluginClientV1` and is registered in Types Registry.

**Main Flow**:
1. The Platform Operator configures the stage kind's `vendor` (and optionally `instance_id`).
2. After restart, the gate calls the stage in its declared order.

**Postconditions**:
- No Consuming Gear changes.

## 9. Acceptance Criteria

- [ ] An oversized request is refused without calling any stage.
- [ ] An undeclared or malformed Fact gives Invalid without calling any stage.
- [ ] A check missing a needed Fact gets a missing-Facts answer once; missing it again gives Unavailable.
- [ ] A Guardrail Stage deny stops the check before the Approval Stage, and no Approval Request is opened.
- [ ] With a stage in the Governance Profile absent, every valid check gives Unavailable.
- [ ] A stage timeout or failure refuses; it never allows.
- [ ] A Preview publishes no event and calls every stage in Preview mode.
- [ ] An anonymous context, a platform-plane call, and a malformed identifier are errors, not outcomes.
- [ ] One event is published per refusing Policy and per shadow denial, none for Allowed or Approval pending.
- [ ] A full queue or absent broker does not change any outcome.
- [ ] An unknown key, a zero numeric setting, two side-effect stages, or an unresolved configured stage fails startup.
- [ ] A Policy change in any stage increases the Applicability Snapshot version and publishes a change notice.
- [ ] In a Product without Approval Service, an operation whose Governance Profile includes the Approval Stage is marked governed in the snapshot and gets Unavailable.

## 10. Dependencies

| Dependency | Description | Criticality |
|---|---|---|
| `types-registry` | Stage discovery, Governed Operation types, event type registration | p1 |
| Guardrail Stage (`policy-engine` by default) | Evaluates Guardrail Policies | p1 |
| Approval Stage (`approval-service`) | Evaluates approvals; optional per Product | p1 for approvals |
| `event-broker` | Receives refusal events and snapshot change notices | p2 |
| ToolKit contract binding | REST and gRPC projections of `AdmissionClientV2` and `GovernanceStagePluginClientV1` | p1 |
| `toolkit-policy-evaluation` | (p2) CEL evaluation of local rules in the Governance Enforcer | p2 |

## 11. Assumptions

- Governance Enforcers pass the end caller's `SecurityContext` unchanged.
- Stages resolve tenancy and apply tenant layering; the gate does not.
- IRM is the first enforcing gear; it migrates from `AdmissionClientV1` to `AdmissionClientV2`.

## 12. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| A stage is slower than its bound | Checks refused as could-not-run | Bounded calls; metric per stage |
| The gate is down | Governed operations stop | Fail closed; stateless scale-out; ungoverned operations never call the gate |
| Events dropped under load | Gaps in the refusal trail | Dropped-events metric; events never gate outcomes |

## 13. Open Questions

- Should a dropped-event alert threshold be recommended for operators?
- When is `AdmissionClientV1` retired?

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **Subsystem**: [Operation Governance PRD](../../../../docs/arch/operation-governance/PRD.md), [DESIGN](../../../../docs/arch/operation-governance/DESIGN.md), [ADR 0001](../../../../docs/arch/operation-governance/ADR/0001-cpt-cf-operation-governance-adr-governance-gate.md), [ADR 0002](../../../../docs/arch/operation-governance/ADR/0002-cpt-cf-operation-governance-adr-fact-collection.md)
