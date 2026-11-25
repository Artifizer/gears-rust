---
description: "Decides how Operation Governance covers every operation through explicit Governed Operation types that are independent of API contracts, checks them in the domain service, and loads Facts only when an applicable Policy reads them."
status: proposed
date: 2026-11-23
---
# Govern Every Operation, Load Facts Only When Read


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1. Eager](#option-1-eager)
  - [Option 2. Applicability index](#option-2-applicability-index)
  - [Option 3. Two-phase check](#option-3-two-phase-check)
  - [Option 4. Remote Fact callbacks](#option-4-remote-fact-callbacks)
  - [Option 5. Referenced Fact sets](#option-5-referenced-fact-sets)
  - [Option 6. Derive from routes and DTO annotations](#option-6-derive-from-routes-and-dto-annotations)
  - [Option 7. Explicit Governed Operation types, checked in the domain service](#option-7-explicit-governed-operation-types-checked-in-the-domain-service)
  - [Option 8. Layered](#option-8-layered)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-operation-governance-adr-fact-collection`

Terms and actors are defined in the [PRD](../PRD.md#14-glossary).

## Context and Problem Statement

Gear Vendors cannot know which operations a Platform Operator will want to govern. Each check needs Facts. Some are in the request; others need a database read or a call to another gear. Policies are written by Tenant Administrators against named Facts and outlive any one version of a gear's API. How can every operation be governable, an ungoverned operation pay almost nothing, a governed one load only the Facts it needs, and a change to a gear's API never silently change what Policies see?

## Decision Drivers

- **Coverage**: the Platform Operator and Tenant Administrators can govern any authenticated operation without waiting for a gear change.
- **No cost without Policies**: no I/O, no network call, and no Fact loading when nothing applies (`cpt-cf-operation-governance-nfr-zero-policy-overhead`).
- **Precision**: a governed operation loads only the Facts that applicable Policies read.
- **Fail closed**: uncertainty about applicability or Facts never skips a check.
- **Contract stability**: renaming a route, adding an API version, or refactoring a DTO never changes a Governed Operation or its Facts.
- **Gear Vendor effort**: little code per operation, and no approval logic.
- **Simplicity**: few moving parts.

## Considered Options

1. **Eager**: the gear collects all Facts before every check.
2. **Applicability index**: a cached, local "is anything configured here?" lookup before collecting Facts.
3. **Two-phase check**: the gate answers with the Facts it is missing; the gear loads them and calls again.
4. **Remote Fact callbacks**: the decision point calls back into the gear for each Fact it reads.
5. **Referenced Fact sets**: each published Policy records which Facts it reads; the gear loads only those.
6. **Derive from routes and DTO annotations**: the route's operation ID is the Governed Operation, fields marked on request DTOs are Facts, and a wrapper at the contract layer runs the check.
7. **Explicit Governed Operation types, checked in the domain service**: the gear declares a GTS type per operation, fills its Facts from domain types, and calls the check before the first side effect.
8. **Layered**: 7 for the contract and check point, a mandatory route step for coverage, 2 for the fast path, 5 for precision, and 3 as the fallback.

| Option | Coverage | No cost | Precision | Fail closed | Stability | Vendor effort | Simplicity | **Total (35)** |
|---|---|---|---|---|---|---|---|---|
| 1. Eager | 2 | 1 | 1 | 5 | 3 | 2 | 5 | **19** |
| 2. Applicability index | 2 | 5 | 2 | 4 | 3 | 4 | 4 | **24** |
| 3. Two-phase check | 2 | 3 | 5 | 5 | 3 | 3 | 3 | **24** |
| 4. Remote Fact callbacks | 2 | 4 | 5 | 3 | 3 | 3 | 2 | **22** |
| 5. Referenced Fact sets | 3 | 4 | 5 | 4 | 3 | 3 | 3 | **25** |
| 6. Routes and DTO annotations | 5 | 4 | 2 | 2 | 1 | 5 | 4 | **23** |
| 7. Explicit types, domain check | 3 | 4 | 4 | 4 | 5 | 3 | 4 | **27** |
| **8. Layered** | **5** | **5** | **5** | **5** | **5** | **4** | **2** | **31** |

Scores are 1 (worst) to 5 (best).

## Decision Outcome

Chosen option: **Option 8, layered**, because it is the only option that meets coverage, no cost, precision, and contract stability together. Each layer is a proven industry pattern:

| Layer | Industry precedent |
|---|---|
| Explicit operation types with typed Facts | AWS IAM actions and condition keys documented per action; Cedar schemas |
| Mandatory route step | ToolKit's own license step; Kubernetes: every API write passes admission |
| Applicability Snapshot | Kubernetes webhook `matchConditions`; feature-flag SDKs evaluate cached rules in memory |
| Referenced Fact sets | OPA reference analysis; Kubernetes ValidatingAdmissionPolicy CEL type-checked against a schema |
| Missing-Fact fallback | SpiceDB caveats: a "conditional" result lists the missing context |

How it works:

1. **Contract.** Each Governed Operation is a GTS type derived from the abstract base `gts.cf.core.admission_control.governed_operation.v1~`, which the Admission Control SDK owns. The gear names it by resource type and action, for example `…governed_operation.v1~cf.file_storage.create_file.v1~`, never by route. The top-level properties of its `facts` schema are its Facts; a Fact ID is one such property. Per-Fact metadata is a GTS trait of the type: Fact Tier, `sensitive`, `approver_visible`, and `binding`. API DTOs carry no governance annotations. REST, gRPC, and in-process calls of one operation map to the same type.
2. **Versioning.** The Facts contract is versioned on its own, not with the API. Policies refer to the operation's family: its type ID without the version. Adding an optional Fact is a compatible change. Renaming, retyping, or removing a Fact needs a new type version; Types Registry refuses a breaking change in place. A renamed Fact is listed in the new version's `replaces` trait; the Governance Enforcer asks the gear for it by its new name, and the gate passes it under both names, so Policies written for the old version keep matching. The gate passes the family to every stage, and stages match Policies and opportunities by family. When a Policy reads a Fact that the current version removed or retyped, the stage reports it when the version is registered, and checks where that Policy applies give Unavailable until it is fixed. A check is never matched against one version's Policies only.
3. **Coverage.** `OperationBuilder` and the routes generated by `#[toolkit::rest_contract]` get a mandatory governance step, in the same type-state style as the license step: `governed::<Op>()` or `not_governed(reason)`. A route without it does not compile. Every governed operation calls the check, even with Envelope Facts only, so a Tenant Administrator can govern it later with no gear change. The SDK conformance kit fails a gear's tests when a governed route reaches its first side effect without a Governance Check for each governed operation it performs (a batch Preview counts as one), and the Governance Enforcer counts such requests at runtime.
4. **Check point.** The domain service calls the Governance Enforcer after Authorization and idempotency replay, and before the first side effect. A replay of an operation that completed returns its stored result and is not checked again. An attempt that was refused or is waiting for approval is not a completed operation: a retry is checked again. The gear's `FactProvider` fills Facts from domain types through a typed Fact API generated from the GTS type, so an API change breaks the gear's build, not Policies.
5. **Fast path.** Each Governance Stage reports, per Governed Operation and tenant scope, whether any Policy or a Default other than Allow applies, and which Facts it reads. Admission Control merges these into a versioned, signed Applicability Snapshot. The Governance Enforcer fetches an entry on a cache miss, reuses it in memory until the Applicability Lease the gate grants with it ends (capped by the gear's maximum), and ends a lease early on an Event Broker notice. If nothing applies, the operation runs with no further governance work. An operation whose Governance Profile names a stage that is not deployed is always marked governed, so its check gives Unavailable instead of being skipped.
6. **Precision.** Each stage computes a Policy's reads set when it is published. CEL reads are always exact. Rego reads are exact only when every input reference resolves statically; a Tenant Rule with an inexact reads set is refused, and an inexact Platform Rule counts as reading every declared Fact. A Policy that reads an undeclared Fact is refused. The Governance Enforcer loads only the Facts in the reads sets of applicable Policies, plus the `approver_visible` Facts when approval may be required. Facts of one tier load in one `FactProvider` call, so the gear can join queries.
7. **Fallback.** The gate computes the needed Facts from its own current plan, not from the gear's snapshot. If any are missing, it answers with their names; the Governance Enforcer loads them and checks once more. A second miss, or a needed Fact that the gear reports as unavailable, gives Unavailable. This also corrects a stale snapshot.
8. **Local rules (p2).** The snapshot may carry side-effect-free CEL rules, cost-checked at publish time, with their reads sets. The Governance Enforcer evaluates them in a sandbox with a cost bound, cheapest Fact Tier first, so a cheap Block avoids loading expensive Facts. Rego runs only at the gate. Each local rule names its evaluator profile version; an older SDK sends the rule to the gate instead.

### Consequences

- **Admission Control SDK** gains the Governed Operation base type and traits, `FactProvider` and its typed Fact API, the Governance Enforcer, and the missing-Facts response of `AdmissionClientV2` ([ADR 0001](./0001-cpt-cf-operation-governance-adr-governance-gate.md)).
- **ToolKit** gains the governance step in `OperationBuilder` and the contract macros. The SDK `test_support` module gains the conformance kit: a fake gate, a plan builder, and a `FactProvider` checker.
- **`toolkit-policy-evaluation`** gains a CEL backend (lifted from Quota Enforcement's bounded CEL profile) and a flag that says whether a document's input references are exact.
- **Governance Stages** report applicability and reads sets and refuse Policies with undeclared Facts or, for Tenant Rules, inexact reads.
- **Admission Control** validates every check against the operation type: the calling gear's identity is mandatory, and that gear must own the type, every provided Fact must be declared and match its schema, and every needed Fact must be present.
- **Facts are asserted by the Consuming Gear.** The gate checks their shape, not their truth. Envelope Facts always come from the security context.
- **Facts are values at check time,** not locks. A Policy must not enforce a counter or limit; limits belong to Quota Enforcement.
- **Approvals** bind to a SHA-256 digest of the operation family and the values of the `binding` Facts that the Approval Stage's own rules read, plus its approver-visible `binding` Facts (`cpt-cf-operation-governance-fr-approval-binding`). Volatile Facts are marked non-binding, so Approvers see them without every change asking for approval again.
- **Policy changes** apply within the Policy Propagation Delay. A missing snapshot entry, or one older than that delay, makes the Governance Enforcer call the Governance Check instead of skipping it.
- **Unaddressed:** platform-plane calls are not Governed Operations (PRD §4.2).

### Confirmation

- A benchmark in the first Consuming Gear shows zero I/O, zero network calls, and at most 0.1 ms p99 added for an ungoverned operation.
- Tests show that a governed operation calls only the `FactProvider` tiers its applicable Policies read, and that a replay of a completed operation is not checked again while a retry of a pending one is.
- Fault tests show that a missing or stale snapshot leads to a full Governance Check, and a twice-missing Fact to Unavailable.
- Renaming a route and adding a REST v2 of an operation leaves its Policies matching, and so does a new type version that renames a Fact.
- A check that names a calling gear other than the type's owner, or no calling gear, gives Invalid.
- A Tenant Administrator blocks an operation its Gear Vendor did not prepare, using Envelope Facts only.

## Pros and Cons of the Options

### Option 1. Eager

- Good, because it is simple and the Facts are always complete.
- Bad, because every governable request pays for Fact loading, even with no Policy.

### Option 2. Applicability index

- Good, because an ungoverned operation costs one in-memory lookup.
- Bad, because a governed operation still loads every Fact, and the index needs a refresh channel and a staleness bound.

### Option 3. Two-phase check

- Good, because only needed Facts are loaded, and the gear needs no knowledge of Policies.
- Bad, because every governed operation that needs a costly Fact makes two calls.

### Option 4. Remote Fact callbacks

- Good, because Facts are pulled exactly when a rule reads them.
- Bad, because network callbacks add latency, coupling, and hard-to-bound timeouts, and the Facts used must still be captured for approval binding and audit.

### Option 5. Referenced Fact sets

- Good, because a governed operation loads exactly the Facts its Policies read.
- Bad, because Rego references are not always exact, and the sets must be kept current.

### Option 6. Derive from routes and DTO annotations

- Good, because every route is governable with no extra declaration.
- Bad, because renaming a route or adding an API version silently detaches its Policies, so the operation runs ungoverned.
- Bad, because a DTO refactor renames, retypes, or removes Facts that Tenant Administrators' Policies read, and one operation has a different Fact set per transport.
- Bad, because a check at the contract layer runs before idempotency replay and before the entity is loaded.

### Option 7. Explicit Governed Operation types, checked in the domain service

- Good, because the governance contract is independent of the API, and an API change breaks the gear's build instead of Policies.
- Good, because rich Facts are cheap at that point, and replays are not checked again.
- Bad, because, alone, coverage depends on each gear calling the check.

### Option 8. Layered

- Good, because each layer removes a weakness of another: stability from 7, coverage from the route step, cost from 2, precision from 5, safety from 3.
- Bad, because it is the most complex option and spans ToolKit, Admission Control, and both stages.

## More Information

- [ToolKit contract binding](../../toolkit-contract-binding/DESIGN.md) generates the routes where the governance step is added; its [versioning ADR](../../toolkit-contract-binding/ADR/0007-cpt-cf-binding-adr-contract-versioning.md) governs API versions, which this decision keeps apart from operation types.
- [ToolKit out-of-process design](../../toolkit-oop/DESIGN.md) covers remote calls and plugin transport.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-cf-operation-governance-fr-operation-coverage` — a mandatory governance step on every route.
- `cpt-cf-operation-governance-fr-governed-operation` — explicit GTS operation types with Facts, versioned apart from the API.
- `cpt-cf-operation-governance-fr-fact-loading` — Facts are loaded only when read, with a missing-Facts fallback.
- `cpt-cf-operation-governance-fr-safe-retries` — replays of completed operations are not checked again.
- `cpt-cf-operation-governance-nfr-zero-policy-overhead` — the in-memory fast path.
- `cpt-cf-operation-governance-nfr-fail-closed` — a missing or stale snapshot falls back to a full check.
- `cpt-cf-operation-governance-component-enforcer` — the component this decision defines.
