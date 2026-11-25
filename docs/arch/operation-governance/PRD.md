---
description: "Master requirements for Operation Governance: admission checks, approvals, and configurable policies applied to operations across Gears."
---

# PRD — Operation Governance


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
  - [5.1 Operations and Checks](#51-operations-and-checks)
  - [5.2 Configurable Policies](#52-configurable-policies)
  - [5.3 Approvals](#53-approvals)
  - [5.4 Explanation and Audit](#54-explanation-and-audit)
- [6. Non-Functional Requirements](#6-non-functional-requirements)
  - [6.1 Subsystem NFRs](#61-subsystem-nfrs)
  - [6.2 NFR Exclusions](#62-nfr-exclusions)
- [7. Public Library Interfaces](#7-public-library-interfaces)
- [8. Use Cases](#8-use-cases)
- [9. Acceptance Criteria](#9-acceptance-criteria)
- [10. Dependencies](#10-dependencies)
- [11. Assumptions](#11-assumptions)
- [12. Risks](#12-risks)
- [13. Open Questions](#13-open-questions)
- [14. Traceability](#14-traceability)

<!-- /toc -->

## 1. Overview

### 1.1 Purpose

Operation Governance is one mechanism for controlling operations across gears: before a [Governed Operation](#14-glossary) runs, its Facts are checked against configurable Policies, in ordered Governance Stages, and the operation gets one Governance Outcome. It runs in addition to Authorization, never instead of it. Policies are set at runtime, without code changes, globally, per tenant, or per actor (user or API client).

The same mechanism serves these purposes:

| Purpose | What it does | Delivered by | Priority |
|---|---|---|---|
| **Admission control** | Refuses an operation that breaks a Guardrail Policy | Guardrail Stage | p1 |
| **Approvals** | Holds an operation until the right Approvers approve it | Approval Stage | p1 |
| **Feature gating** | Turns an operation on or off per tenant or actor, for example during a rollout | Guardrail Policies on Envelope Facts | p1 |
| **Invocation audit** | Records who invoked which operation, per tenant or actor, with its Facts | Asynchronous checks | p3 |
| **Custom webhooks** | Lets a Product or customer decide in its own service or function | External Governance Stages | p3 |
| **License checks** | Refuses an operation the tenant is not licensed for | A licensing Governance Stage that asks License Resolver | p3 |

New purposes are added as new Governance Stages or Policy kinds, without changing Consuming Gears.

Three gears and one SDK component deliver these capabilities together. Admission Control runs every Governance Check and combines its Governance Stages. Policy Engine provides the Guardrail Stage. Approval Service provides the Approval Stage. The Governance Enforcer in each Consuming Gear calls them. This PRD is the master document for the subsystem. It defines the shared actors, terms, and requirements. Each gear PRD defines that gear's own requirements (see [§10](#10-dependencies)).

### 1.2 Background / Problem Statement

Many gears need the same controls: block a forbidden operation, ask a manager to approve a large purchase, or let a resource owner decide who may use a resource. Without a shared subsystem, every gear builds its own rules, approval state, tenant handling, and audit, and each behaves differently.

The controls also interact. An operation can pass Authorization and admission and still need approval. It can be approved and then blocked by a new Guardrail Policy. Without one place that combines the checks, gears report different results, open approval requests for operations that can never run, or let resumed work skip a check.

Gear Vendors also cannot know in advance which operations a Platform Operator will want to govern, and collecting Facts can cost a database read. Governance must therefore cover every operation without Gear Vendor guesswork, and cost almost nothing when no Policy applies.

### 1.3 Goals (Business Outcomes)

- **Admission**: no Governed Operation runs after a Guardrail Policy blocks it or a required check fails (zero exceptions).
- **Approvals**: a Gear Vendor adds an approval step to an operation without writing approval logic, and Approvers see every request they may decide, with the Facts they need.
- **Configurable policies**: the Platform Operator and Tenant Administrators can govern any authenticated operation of any Consuming Gear and change Policies at runtime, without a restart or a release.
- **No cost without Policies**: an operation that no Policy governs pays no I/O and no network call for governance.
- **One mechanism**: every purpose in §1.1 is delivered through the same Governed Operations, Facts, and Governance Check; a new purpose needs no change in Consuming Gears.
- **Clear outcomes**: the Requester always gets exactly one Governance Outcome with a reason. An Auditor can trace every outcome to the Policy versions and Decisions that produced it.

### 1.4 Glossary

This glossary is the single source of terms for Operation Governance. [USECASES.md](./USECASES.md), [DESIGN.md](./DESIGN.md), and the [ADRs](./ADR/) use these terms.

**Platform and tenants**

| Term | Definition |
|---|---|
| Product | A set of gears that a Platform Operator assembles, configures, and runs as one platform installation. A Product always includes Admission Control and a Guardrail Stage. Approval Service is optional. |
| Tenant | A unit of ownership and data isolation. Tenants form one tree. See the [Tenant Model](../authorization/TENANT_MODEL.md). |
| Root Tenant | The top tenant of the tree. The Platform Operator owns it. |
| Reseller | A tenant that sells the Product to its own child tenants and may govern them. |
| Managed Tenant | A tenant whose ancestors may govern it: their Tenant Rules apply to it. |
| Self-Managed Tenant | A tenant behind a barrier: its ancestors cannot see or govern it. Only Platform Rules and its own Tenant Rules apply. |
| Authorization | The platform check of whether the Requester may call an operation at all. AuthZ Resolver decides it. It is not part of Operation Governance, but it always runs first. |

**Operations and Facts**

| Term | Definition |
|---|---|
| Governed Operation | An action of a Consuming Gear on a kind of resource that Operation Governance can check, for example "release a purchase order". Each one is declared as its own GTS type, named by resource type and action, and is independent of the gear's API: routes, API versions, and request bodies do not define it. Every authenticated operation is a Governed Operation unless it is excluded with a reason. |
| Facts | Values that describe one attempt of a Governed Operation, for example an amount, a vendor, or changed paths. Each Fact has a stable name in its Governed Operation type. Policies use them in conditions, and Approvers see them. Admission Control calls them properties; Approval Service calls them context attributes. |
| Fact Tier | How costly a Fact is to get: **request** (already in the request or the security context), **computed** (derived in memory), or **costly** (needs a database read or a call to another gear). |
| Envelope Facts | The request-tier Facts that every Governed Operation has without Gear Vendor work: the Requester (its ID, its kind — user or API client — and its tenant), the Governed Operation, and the target resource type, ID, and tenant. |
| Resource Version | The version of the target resource that a check, an Approval Request, or a Decision refers to. A new version needs a new approval. |
| Plan | A set of Governed Operations that must be admitted as a whole: all of them run or none, for example an infrastructure change plan. |

**Checks**

| Term | Definition |
|---|---|
| Governance Check | One decision by Admission Control on one Governed Operation: it runs every applicable Governance Stage in order and returns one Governance Outcome. |
| Governance Stage | A plugin of Admission Control that evaluates one kind of Policy. Stages run in a fixed order: the Guardrail Stage, then the Approval Stage. |
| Guardrail Stage | The Governance Stage that evaluates Guardrail Policies. Policy Engine provides it by default. |
| Approval Stage | The Governance Stage that evaluates Approval Rules, Approval Policies, Auto-Approval Rules, and Decisions, and can open an Approval Request. Approval Service provides it. It always runs last. |
| Governance Profile | The Governance Stages that apply to a Governed Operation. It comes from trusted configuration, never from the request. |
| Governance Enforcer | The Admission Control SDK component inside each Consuming Gear that enforces governance: it checks the Applicability Snapshot, asks the gear for only the Facts that are needed, calls the Governance Check, and checks again before parked work resumes. |
| Applicability Snapshot | A versioned, signed summary, cached by the Governance Enforcer, of which Governed Operations have an applicable Policy or a Default other than Allow, per tenant, and which Facts those Policies read. |
| Preview | A Governance Check that changes nothing: it opens no Approval Request and writes no audit record or event. It answers "what would happen?". |

**Policies**

| Term | Definition |
|---|---|
| Policy | Any configured, versioned rule that governs operations: a Guardrail Policy, an Approval Rule, an Approval Policy, or an Auto-Approval Rule. |
| Guardrail Policy | A Policy that blocks matching operations, with no approval path. Policy Engine stores and evaluates it. |
| Approval Rule | A Policy that gives matching operations one action: **Block**, **Require approval**, or **Allow**. Allow means "no approval needed"; it never overrides a block or Authorization. Approval Service stores and evaluates it. |
| Approval Policy | A Policy that says who must approve and how many: minimum approvals, mandatory Approvers, eligible Approvers, and ordered stages of Approvers. |
| Auto-Approval Rule | A Policy that approves a request without human review when its condition on the Facts holds, for example "amount below 1,000". It never overrides a block. |
| Platform Rule | A Policy that the Platform Operator sets in Product configuration. It applies to every tenant, including Self-Managed Tenants, and no tenant can loosen it. |
| Tenant Rule | A Policy that a Tenant Administrator sets at runtime. It applies to that tenant and its descendants, up to a Self-Managed Tenant. |
| Policy Scope | Whom a Policy applies to: **global** (every tenant; a Platform Rule), or one **tenant** and its Managed descendants (a Tenant Rule). Either can be narrowed to **actors**: named users, named API clients, or groups of them. |
| Default | The result used only when no rule decides: the Platform Default from Product configuration, or else the Operation Default from the Gear Vendor. A Tenant Rule replaces a Default. When nothing is declared, the Operation Default is Allow. |
| Trial Mode | A Policy runs on real checks, and its would-be result is reported to Tenant Administrators, but it never blocks or requires approval. Policy Engine calls this shadow mode. |
| Applicability Lease | How long a Governance Enforcer may reuse an answer about which Policies apply to an operation before it asks again. It is granted by the backend that answers and is never longer than the Policy Propagation Delay. |
| Policy Propagation Delay | The configured maximum time between committing a Policy change, or a change between managed and self-managed, and every Governance Enforcer applying it. |

**Approvals**

| Term | Definition |
|---|---|
| Approval Request | An open case for one Governed Operation on one Resource Version with one set of Facts, waiting for Decisions. It has a reference that the Requester and the Consuming Gear can follow. |
| Decision | An Approver's approve or deny, with a reason. |
| Standing Approval | An approve Decision that covers repeated operations until it is revoked or expires. It is possible only where the Governed Operation permits it. |
| Approval Lease | How long a Consuming Gear may rely on an approval result before it checks again. It bounds how long a revoked approval keeps working. |
| Separation of Duties | The rule that the Requester, and people named in the Facts such as a preparer or an author, cannot approve the operation. |

**Outcomes and audit**

| Term | Definition |
|---|---|
| Governance Outcome | The one combined result that the Consuming Gear gets for a Governed Operation: Allowed, Blocked, Approval required, Approval pending, Invalid, or Unavailable ([§5.1](#governance-outcome)). |
| Refusal Reason | Why the outcome is not Allowed: no permission, blocked by rule, denied by Approver, approval missing, invalid Facts, or unavailable. |
| Correlation ID | An ID that links all checks, Approval Requests, Decisions, and audit records of one operation, including its resumed attempts. It is the platform request ID when one exists. |
| Priority | **p1** must have in the first release; **p2** important soon after; **p3** valuable for specific customers or compliance programmes; **p4** nice to have; **p5** possible future work. |

## 2. Actors

Gear PRDs use other names for some of these actors. The name used here is the one used across Operation Governance.

### 2.1 Human Actors

#### Platform Operator

**ID**: `cpt-cf-operation-governance-actor-platform-operator`

- **Role**: Assembles gears into a Product, configures it, and runs it for all tenants. Sets Platform Rules, Platform Defaults, Governance Profiles, and the Policy Propagation Delay, and decides whether the Product includes Approval Service. Called Platform Vendor in the Approval Service PRD.
- **Needs**: Rules that every tenant must follow, on any operation, and a controlled way to change them.

#### Gear Vendor

**ID**: `cpt-cf-operation-governance-actor-gear-vendor`

- **Role**: Builds a Consuming Gear. Declares a Governed Operation type for each operation, with Facts, Fact Tiers, and Operation Defaults, fills the Facts from the gear's own data, and excludes an operation from governance only with a reason.
- **Needs**: Governance on every operation with no approval logic in the gear, and no cost when nobody governs an operation.

#### Tenant Administrator

**ID**: `cpt-cf-operation-governance-actor-tenant-admin`

- **Role**: Manages the Tenant Rules of a tenant and its Managed Tenants: writes, tests, publishes, rolls back, and retires them. Manages Approvers and Standing Approvals. Called Target Platform Administrator in the Approval Service PRD, and Policy Author or Tenant Policy Administrator in the Policy Engine PRD.
- **Needs**: Express rules in business terms, test them before they apply, and undo a bad change quickly.

#### Approver

**ID**: `cpt-cf-operation-governance-actor-approver`

- **Role**: A user who may decide Approval Requests for a Governed Operation, for example a line manager, Finance, Legal, a repository maintainer, or the owner of a resource.
- **Needs**: See what is asked and why, and decide quickly.

#### Requester

**ID**: `cpt-cf-operation-governance-actor-requester`

- **Role**: An end user or API client who starts a Governed Operation through a Consuming Gear.
- **Needs**: Know at once whether the operation may proceed, why not, and what happens next.

#### Auditor

**ID**: `cpt-cf-operation-governance-actor-auditor`

- **Role**: A compliance or security reviewer.
- **Needs**: Prove who allowed what, under which Policy version, and who changed the Policies.

### 2.2 System Actors

#### Consuming Gear

**ID**: `cpt-cf-operation-governance-actor-consuming-gear`

- **Role**: A gear that owns a Governed Operation, for example Infrastructure Resource Manager (IRM), Model Registry, or Orders Workflow. Through its Governance Enforcer it gets one Governance Outcome and decides whether to run the operation, wait, or stop. Called enforcing gear in the Admission Control and Policy Engine PRDs, and integrated gear in the Approval Service PRD.

#### AuthZ Resolver

**ID**: `cpt-cf-operation-governance-actor-authz-resolver`

- **Role**: The platform policy decision point for Authorization. See the [Authorization design](../authorization/DESIGN.md).

#### Tenant Resolver

**ID**: `cpt-cf-operation-governance-actor-tenant-resolver`

- **Role**: Provides the tenant tree and Self-Managed Tenant barriers to the Governance Stages.

#### Event Broker

**ID**: `cpt-cf-operation-governance-actor-event-broker`

- **Role**: Delivers refusal events, Approval Request events, and Applicability Snapshot change notices.

#### Resource Group

**ID**: `cpt-cf-operation-governance-actor-resource-group`

- **Role**: Provides group membership when Approvers or Standing Approvals are defined by group.

## 3. Operational Concept & Environment

Platform runtime, architecture, and baselines are defined in the [Architecture Manifest](../../ARCHITECTURE_MANIFEST.md) and the [project guidelines](../../../guidelines/README.md). Authorization is defined in the [Authorization design](../authorization/DESIGN.md), and tenants in the [Tenant Model](../authorization/TENANT_MODEL.md). Operation Governance works in every ToolKit deployment profile (Embedded, Self-Hosted, and Kubernetes Native) defined in the [ToolKit out-of-process PRD](../toolkit-oop/PRD.md).

**Deployments.** The Platform Operator chooses one of two deployments per Product:

| Deployment | Gears | What works |
|---|---|---|
| Admission only | Admission Control with a Guardrail Stage (Policy Engine by default) | Guardrail Policies. A Governed Operation whose Governance Profile includes the Approval Stage gets **Unavailable**, never Allowed. |
| Admission and approval | Admission Control, Policy Engine, and Approval Service | All capabilities. |

**Check flow.** For every Governed Operation:

1. The platform authenticates the Requester, and AuthZ Resolver decides Authorization. A repeated request whose operation already completed returns its stored result without a new check.
2. The Governance Enforcer reads the Applicability Snapshot. If no Policy and no Default other than Allow applies, the operation runs with no further governance work.
3. Otherwise the Governance Enforcer gets only the Facts that the applicable Policies read and calls one Governance Check. If Admission Control needs Facts that were not sent, it names them, and the Governance Enforcer sends them once.
4. Admission Control runs the applicable Governance Stages in order, Guardrail Stage first and Approval Stage last, and returns one Governance Outcome.
5. The Consuming Gear runs the operation only when the outcome is Allowed. On Approval pending it parks the work and checks again before resuming.

Authorization and Operation Governance are independent. Operation Governance never allows what Authorization denies. Authorization never uses Guardrail Policies, Approval Rules, or Decisions as input.

## 4. Scope

### 4.1 In Scope

- Coverage of every authenticated operation, Fact declaration and versioning, and Fact loading cost.
- The Governance Outcome, its Refusal Reasons, and how results of all Governance Stages combine.
- The order of checks, safe failure, retries, and checking again before waiting work runs.
- Previews.
- Shared rules for Policies: kinds, layering across the tenant tree, Self-Managed Tenants, lifecycle, propagation, and Trial Mode.
- Shared rules for approvals: requests, Approver eligibility, binding to what was approved, Standing Approvals, and revocation.
- Explanation and audit of outcomes and Policy changes.

### 4.2 Out of Scope

- Authorization itself: AuthZ Resolver owns it.
- Running, waiting for, resuming, or timing out the operation: the Consuming Gear owns it.
- Calls between gears on the platform plane (service identity, no tenant): they are not Governed Operations.
- Delivery of notifications, reminders, and escalation timers.
- Quota, capacity, and queue-depth limits: owned by their own gears, such as Quota Enforcement, not by Policies. Facts are read without locks, so a Policy cannot enforce a limit under concurrent operations.
- Resource protection flags in IRM, such as no-delete: resource state owned by IRM.
- Approving operations on resources that do not exist yet: the Consuming Gear approves the container or a draft resource instead.
- Deputies for absent Approvers: approval by group covers absence.
- Emergency bypass ("break-glass"): future scope of Approval Service.
- Feature flags with typed values, variants, or experiments: owned by the settings service. Operation Governance only allows or refuses an operation, and fails closed.
- Licenses and grants themselves: owned by License Resolver. A licensing Governance Stage only asks it.

## 5. Functional Requirements

> **Testing strategy**: Each gear verifies its share with automated tests. Each Consuming Gear verifies the combined behaviour at its operation boundary with the acceptance criteria in [§9](#9-acceptance-criteria).

Each requirement lists the gear requirements that realize it under **Realized by**. Requirements that need new ToolKit or gear capabilities point to [DESIGN §4](./DESIGN.md#4-additional-context).

### 5.1 Operations and Checks

#### Operation Coverage

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-operation-coverage`

Every authenticated operation of a Consuming Gear on the tenant plane **MUST** either name its Governed Operation or be excluded explicitly, with a recorded reason; an operation that does neither **MUST NOT** build. Every Governed Operation **MUST** be checked, even with Envelope Facts only, so that the Platform Operator and Tenant Administrators can govern it later without a gear change. Excluded operations **MUST** be listed for the Platform Operator.

- **Rationale**: Gear Vendors cannot predict which operations a Platform Operator will want to govern.
- **Actors**: `cpt-cf-operation-governance-actor-gear-vendor`, `cpt-cf-operation-governance-actor-platform-operator`
- **Realized by**: the governance step of ToolKit operation declarations and the Governance Enforcer ([ADR 0002](./ADR/0002-cpt-cf-operation-governance-adr-fact-collection.md)).

#### Governed Operation Declaration

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-governed-operation`

The Gear Vendor **MUST** be able to declare for a Governed Operation: Facts with their Fact Tier, whether each Fact is sensitive, shown to Approvers, or part of the approval binding, an Operation Default, whether Standing Approvals are allowed, and who may decide (any eligible Approver, or only the tenant that owns the resource). A change to the gear's API **MUST NOT** change a Governed Operation or its Facts. An undeclared or malformed Fact **MUST** give **Invalid**, and a Fact that an applicable Policy reads but the gear does not provide **MUST** give **Unavailable**, never Allowed. A new gear version **MUST** be able to add an optional Fact without breaking Policies. Renaming, retyping, or removing a Fact **MUST** need a new version of the Governed Operation. Policies **MUST** keep matching every version of their Governed Operation: a renamed Fact **MUST** stay readable under its old name, and a Policy that reads a removed or retyped Fact **MUST** be reported to its Tenant Administrators and make the operation **Unavailable**, never Allowed, until it is fixed. Only the Consuming Gear that owns a Governed Operation **MUST** be able to check it.

- **Rationale**: Policies can only be written, validated, and shown to Approvers for operations and Facts that are known in advance. Policies outlive any one version of a gear's API.
- **Actors**: `cpt-cf-operation-governance-actor-gear-vendor`, `cpt-cf-operation-governance-actor-tenant-admin`
- **Realized by**: `cpt-cf-approval-service-fr-register-req-type`, `cpt-cf-approval-service-fr-context-attributes`, `cpt-cf-approval-service-fr-subject`, `cpt-cf-policy-engine-fr-content-validation`

#### Facts Loaded Only When Read

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-fact-loading`

The Governance Enforcer **MUST NOT** get a computed or costly Fact unless an applicable Policy reads it or an Approver must see it. Publishing a Policy **MUST** be refused if it reads an undeclared Fact, or, for a Tenant Rule, if the Facts it reads cannot be determined exactly. When the Facts needed by the current Policies were not all sent, Admission Control **MUST** name the missing Facts once, so that the Governance Enforcer can get them and check again.

- **Rationale**: Facts can cost a database read; most operations need none of them. A Tenant Rule that reads every Fact would make every check of every tenant pay for all of them.
- **Actors**: `cpt-cf-operation-governance-actor-consuming-gear`, `cpt-cf-operation-governance-actor-gear-vendor`, `cpt-cf-operation-governance-actor-tenant-admin`
- **Realized by**: Governance Enforcer; Applicability Snapshot from each Governance Stage ([ADR 0002](./ADR/0002-cpt-cf-operation-governance-adr-fact-collection.md)).

#### Governance Profile

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-governance-profile`

The Governance Profile of each Governed Operation **MUST** come from trusted configuration: the Gear Vendor's declaration and the Platform Operator's Product configuration. Request input **MUST NOT** change it. If the Governance Profile includes the Approval Stage and the Product has no Approval Service, the outcome **MUST** be **Unavailable**.

- **Rationale**: If a caller could choose which checks apply, it could skip them.
- **Actors**: `cpt-cf-operation-governance-actor-platform-operator`, `cpt-cf-operation-governance-actor-consuming-gear`

#### Check Order

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-check-order`

Authorization **MUST** run before the Governance Check, and the Governance Check **MUST** run before the operation's first side effect. Admission Control **MUST** run the applicable Governance Stages in a fixed order, stages without side effects first and the Approval Stage last, and **MUST** stop at the first stage that does not allow the operation. An Approval Request **MUST NOT** be opened unless Authorization and every earlier stage allow the operation. Every stage of one Governance Check **MUST** see the same Requester, target resource, Resource Version, Facts, and Correlation ID.

- **Rationale**: Nobody should be asked to approve an operation that cannot run. One place that orders and combines the stages gives every Consuming Gear the same behaviour.
- **Actors**: `cpt-cf-operation-governance-actor-consuming-gear`, `cpt-cf-operation-governance-actor-requester`
- **Realized by**: `cpt-cf-admission-control-fr-decision-order`, `cpt-cf-approval-service-fr-verdict-resolution`; ordered stages are new in Admission Control ([ADR 0001](./ADR/0001-cpt-cf-operation-governance-adr-governance-gate.md)).

#### Governance Outcome

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-governance-outcome`

Each Governed Operation attempt **MUST** get exactly one Governance Outcome:

| Outcome | May proceed | Meaning |
|---|---|---|
| **Allowed** | yes | Every applicable check allows the operation, or no Policy applies. |
| **Blocked** | no | Authorization denies it, a Guardrail Policy or Approval Rule blocks it, or an Approver denies it. |
| **Approval required** | no | Approval is needed and no Approval Request is open for this Resource Version and Facts. A Preview returns this instead of opening a request. |
| **Approval pending** | no | Approval is needed and an Approval Request is open. The outcome carries the request reference and the unmet requirements. |
| **Invalid** | no | The operation, the resource type, or the Facts are unknown, missing, malformed, or too large. |
| **Unavailable** | no | A required check cannot give a reliable answer, or its gear is not deployed. The outcome says whether a retry can help. |

- **Rationale**: The Requester and the Consuming Gear need one answer that tells them whether to proceed, fix the input, wait, or stop.
- **Actors**: `cpt-cf-operation-governance-actor-consuming-gear`, `cpt-cf-operation-governance-actor-requester`
- **Realized by**: `cpt-cf-admission-control-fr-refusal-cause`, `cpt-cf-approval-service-fr-verify-approval`

#### Refusal Reasons

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-refusal-reasons`

Every outcome other than Allowed **MUST** carry a Refusal Reason, a machine-readable code, and a readable message. A Tenant Administrator **MUST** be able to attach a message to a blocking Policy, and the Requester **MUST** see that message when the Policy blocks them. A denial by an Approver **MUST** carry the Approver's reason.

- **Rationale**: The Requester needs to know what to do next, and an Auditor needs to know which rule or Decision refused the operation.
- **Actors**: `cpt-cf-operation-governance-actor-requester`, `cpt-cf-operation-governance-actor-tenant-admin`
- **Realized by**: `cpt-cf-approval-service-fr-refusal-reasons`, `cpt-cf-policy-engine-fr-denial-reason`, `cpt-cf-admission-control-fr-refusal-cause`

#### Safe Retries

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-safe-retries`

Repeating a Governance Check with the same inputs **MUST** give the same outcome, as long as no Policy, Decision, or tenant changed in between. It **MUST NOT** open a second Approval Request or send a second notification for the same Resource Version and Facts. A repeated request whose operation already completed, under its idempotency key, **MUST** return its stored result without a new Governance Check. A retry of an attempt that was refused or is waiting for approval **MUST** be checked again.

- **Rationale**: Consuming Gears retry on timeouts; retries must not flood Approvers, and a retry of finished work must not be refused after a Policy change.
- **Actors**: `cpt-cf-operation-governance-actor-consuming-gear`
- **Realized by**: `cpt-cf-approval-service-fr-create-request`

#### Check Again Before Running

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-recheck`

Before work that waited for approval runs, and whenever an Approval Lease has ended, the Governance Enforcer **MUST** run Authorization and the Governance Check again with the current Resource Version and Facts. A new refusal **MUST** win over an earlier approval.

- **Rationale**: Policies, Facts, and Decisions can change while work waits. The check that matters is the one at the time the operation runs.
- **Actors**: `cpt-cf-operation-governance-actor-consuming-gear`
- **Realized by**: `cpt-cf-approval-service-fr-approval-lease`

#### Plan Admission

- [ ] `p2` - **ID**: `cpt-cf-operation-governance-fr-plan`

For a Plan, the Consuming Gear **MUST** get a Governance Outcome for every operation in the Plan before it changes anything. If any operation does not get Allowed, no operation in the Plan **MUST** run.

- **Rationale**: A half-applied infrastructure change can leave resources broken.
- **Actors**: `cpt-cf-operation-governance-actor-consuming-gear`

#### Preview

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-preview`

A Consuming Gear **MUST** be able to preview the Governance Outcome of a Governed Operation, for one resource or for many resources in one call. With the same inputs and the same Policies and Decisions, a Preview **MUST** give the same outcome as the real check, except that it gives Approval required instead of opening a request. A Preview **MUST NOT** open an Approval Request or write an audit record or event in any Governance Stage.

- **Rationale**: A UI hides buttons for operations that would be blocked. A list page shows only resources the Requester may use. An infrastructure plan is checked before it is applied.
- **Actors**: `cpt-cf-operation-governance-actor-consuming-gear`, `cpt-cf-operation-governance-actor-requester`
- **Realized by**: `cpt-cf-approval-service-fr-verify-approval`, `cpt-cf-approval-service-fr-batch-verify`; a preview mode is new in Admission Control ([ADR 0001](./ADR/0001-cpt-cf-operation-governance-adr-governance-gate.md)).

### 5.2 Configurable Policies

#### Policy Kinds

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-policy-kinds`

The subsystem **MUST** support Guardrail Policies, Approval Rules, and Approval Policies, and (p2) Auto-Approval Rules. A Policy **MUST** be able to match a kind of resource or one resource. A Policy **MUST** have a Policy Scope: global or one tenant, optionally narrowed to actors (named users, named API clients, or groups); a Policy narrowed to actors applies to nobody else. Guardrail Policies, and (p2) Approval Rules and Approval Policies, **MUST** be able to use conditions on Facts: combined conditions, inclusive thresholds, and list and pattern matches. Money amounts in different currencies **MUST NOT** be compared directly. Approval Policies **MUST** support a minimum number of distinct Approvers and mandatory Approvers, and (p2) ordered stages of Approvers and Approvers named by group.

- **Rationale**: Block, require approval, and decide who approves cover the governance needs of the Consuming Gears listed in [USECASES.md](./USECASES.md).
- **Actors**: `cpt-cf-operation-governance-actor-tenant-admin`, `cpt-cf-operation-governance-actor-platform-operator`
- **Realized by**: `cpt-cf-policy-engine-fr-bundle-composition`, `cpt-cf-approval-service-fr-approval-rules`, `cpt-cf-approval-service-fr-approval-policies`, `cpt-cf-approval-service-fr-auto-approval`, `cpt-cf-approval-service-fr-conditional-policies`, `cpt-cf-approval-service-fr-staged-policies`, `cpt-cf-approval-service-fr-group-approvers`

#### Policy Layers and Precedence

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-policy-layers`

Policies **MUST** combine across the tenant tree as follows:

1. Platform Rules apply to every tenant. No Tenant Rule can loosen them.
2. A Tenant Rule applies to its tenant and its Managed descendants. A descendant can only add stricter rules.
3. Within those limits, a tenant's own rule decides for that tenant.
4. A Default applies only when no rule decides.
5. When rules disagree, Block wins over Require approval, and Require approval wins over Allow.
6. Narrowing a rule to actors only limits whom it applies to. An actor-scoped Allow never loosens a broader Block or Require approval, by rule 5.

(p3) A parent tenant **MUST** be able to let its children relax one named rule explicitly.

- **Rationale**: The platform and parent tenants can always restrict, never be loosened by a child. Within those limits a tenant governs itself.
- **Actors**: `cpt-cf-operation-governance-actor-platform-operator`, `cpt-cf-operation-governance-actor-tenant-admin`
- **Realized by**: `cpt-cf-approval-service-fr-rule-layers`, `cpt-cf-approval-service-fr-tenant-hierarchy`, `cpt-cf-approval-service-fr-delegable-rules`, `cpt-cf-policy-engine-fr-builtin-policies`, `cpt-cf-policy-engine-fr-applicable-set`, `cpt-cf-policy-engine-fr-denial-precedence`

#### Self-Managed Tenants

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-self-managed`

Only Platform Rules and its own Tenant Rules **MUST** apply to a Self-Managed Tenant. Administrators and Approvers of its ancestors **MUST NOT** see or decide its Approval Requests. A change between managed and self-managed **MUST** apply within the Policy Propagation Delay.

- **Rationale**: A self-managed customer runs independently of its parent.
- **Actors**: `cpt-cf-operation-governance-actor-tenant-admin`, `cpt-cf-operation-governance-actor-tenant-resolver`
- **Realized by**: `cpt-cf-approval-service-fr-tenant-barriers`, `cpt-cf-policy-engine-fr-inheritance-barriers`

#### Policy Lifecycle

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-policy-lifecycle`

Every Policy change **MUST** be recorded as a new version with its author, time, and reason, and **MUST** apply within the Policy Propagation Delay, without a restart. A Tenant Administrator **MUST** be able to retire a Policy. (p2) A Tenant Administrator **MUST** be able to save a draft that does not affect outcomes, see every validation error at once, compare two versions, and roll back in one action. A change based on an outdated version **MUST** be refused. Platform Rules **MUST** change only through Product configuration.

- **Rationale**: Policies change while work is in progress. Administrators need to see who changed what and to undo a bad change at once.
- **Actors**: `cpt-cf-operation-governance-actor-tenant-admin`, `cpt-cf-operation-governance-actor-platform-operator`, `cpt-cf-operation-governance-actor-auditor`
- **Realized by**: `cpt-cf-policy-engine-fr-lifecycle-states`, `cpt-cf-policy-engine-fr-active-content-loading`, `cpt-cf-approval-service-fr-governance-versions`

#### Policy Changes and Waiting Work

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-change-effect`

When a Policy changes while Approval Requests are open: a stricter Policy **MUST** apply to open requests, and Decisions already given **MUST** still count while their Approver remains eligible. Requests that are no longer needed **MUST** close within a configured, bounded delay. Operations that already ran **MUST NOT** be affected.

- **Rationale**: Changing a Policy must have a predictable effect on work in progress.
- **Actors**: `cpt-cf-operation-governance-actor-tenant-admin`, `cpt-cf-operation-governance-actor-requester`
- **Realized by**: `cpt-cf-approval-service-fr-approval-policies`, `cpt-cf-approval-service-fr-request-reconciliation`, `cpt-cf-approval-service-fr-policy-snapshot`

#### Asynchronous Checks

- [ ] `p3` - **ID**: `cpt-cf-operation-governance-fr-async-checks`

A Policy **MUST** be able to run as an asynchronous check: it is evaluated after the operation ran, off the request path, on the Facts the operation had at check time, and it never delays or refuses that operation. Its result **MUST** be recorded with the Correlation ID and published as an event, so that a Tenant Administrator, an Auditor, or a Consuming Gear can act on it, for example by flagging or reverting the change. A Fact that changes after the operation ran **MUST NOT** change the result of its asynchronous check.

- **Rationale**: Some checks are too slow for the request path (a scan, a call to an external system) or only need to detect, not prevent.
- **Actors**: `cpt-cf-operation-governance-actor-tenant-admin`, `cpt-cf-operation-governance-actor-auditor`, `cpt-cf-operation-governance-actor-consuming-gear`

#### Policies on Operation Patterns

- [ ] `p3` - **ID**: `cpt-cf-operation-governance-fr-operation-patterns`

A Policy **MUST** be able to apply to a GTS pattern of Governed Operations instead of one operation, for example every operation of one gear (`gts.cf.core.og.governed_operation.v1~x.object_storage.*`). Publishing such a Policy **MUST** be refused unless every Fact it reads is declared, with the same type, by every Governed Operation the pattern matches among those visible in Types Registry. When a newly registered operation matches the pattern but does not declare a Fact the Policy reads, the Tenant Administrator **MUST** be told, and checks of that operation where the Policy applies **MUST** give Unavailable, never Allowed, until the Policy or the operation is fixed. A Policy that reads only Envelope Facts **MUST** match any operation.

- **Rationale**: Cross-cutting rules (freeze all writes of a gear, block one API client everywhere) should not be repeated per operation.
- **Actors**: `cpt-cf-operation-governance-actor-tenant-admin`, `cpt-cf-operation-governance-actor-platform-operator`

#### Trial Mode and Policy Preview

- [ ] `p3` - **ID**: `cpt-cf-operation-governance-fr-trial-mode`

A Tenant Administrator **MUST** be able to evaluate a draft Policy against sample Facts without any effect, and to run a Policy in Trial Mode. Trial Mode results **MUST** be reported apart from real refusals. Switching from Trial Mode to enforced **MUST** apply within the Policy Propagation Delay. Policy Engine already provides Trial Mode for Guardrail Policies in its first release.

- **Rationale**: A new restriction can block a whole tenant. Administrators need to see its effect on real traffic first.
- **Actors**: `cpt-cf-operation-governance-actor-tenant-admin`
- **Realized by**: `cpt-cf-policy-engine-fr-non-enforcing-assignment`, `cpt-cf-policy-engine-fr-publish-dry-run`, `cpt-cf-approval-service-fr-governance-trial`

### 5.3 Approvals

#### Approval Requests

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-approval-requests`

An Approval Request **MUST** be opened only by a real (not Preview) Governance Check whose Approval Stage finds that approval is needed. The Requester **MUST** see its reference, who must approve, and the current stage of Approvers, and **MUST** be able to add a justification and withdraw the request. The request **MUST** close when it is decided, withdrawn, no longer needed, replaced by a new Resource Version, or when the resource is removed.

- **Rationale**: Requesters need to know what is needed and follow progress; stale requests must leave Approvers' lists.
- **Actors**: `cpt-cf-operation-governance-actor-requester`, `cpt-cf-operation-governance-actor-consuming-gear`
- **Realized by**: `cpt-cf-approval-service-fr-create-request`, `cpt-cf-approval-service-fr-request-reconciliation`

#### Approver Eligibility and Decisions

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-decisions`

Only eligible Approvers **MUST** count toward an Approval Policy. Separation of Duties **MUST** apply: the Requester can never approve, and (p2) people named in the Facts can be excluded. Where the Governed Operation allows only the resource owner to decide, only Approvers of the tenant that owns the resource **MUST** be able to approve. Every Decision **MUST** carry a reason. Any applicable deny Decision **MUST** block, even if others approved. Approvers **MUST** be able to see only requests they may decide, withdraw their own Decision, and decide many requests at once (all recorded, or none with a reason per item). A Tenant Administrator **MUST** be able to revoke any Decision in their scope.

- **Rationale**: Four-eyes control only works if the wrong people cannot approve and a single "no" stops the operation.
- **Actors**: `cpt-cf-operation-governance-actor-approver`, `cpt-cf-operation-governance-actor-tenant-admin`
- **Realized by**: `cpt-cf-approval-service-fr-submit-decision`, `cpt-cf-approval-service-fr-approver-exclusions`, `cpt-cf-approval-service-fr-validate-eligibility`, `cpt-cf-approval-service-fr-administration`, `cpt-cf-approval-service-fr-bulk-approve`

#### Approval Bound to What Was Approved

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-approval-binding`

An approval given for a request **MUST** count only for the same Governed Operation, resource, Resource Version, and values of its binding Facts: the Facts that approval rules read or that Approvers see, except those the Gear Vendor declares as not binding because they change often, such as counts. A change to a Policy of another kind **MUST NOT** change the binding. If any of them changes, the operation **MUST** need approval again, or be Blocked if a blocking Policy now matches.

- **Rationale**: Approving a purchase order of 5,000 must not approve the same order edited to 6,000.
- **Actors**: `cpt-cf-operation-governance-actor-approver`, `cpt-cf-operation-governance-actor-consuming-gear`
- **Realized by**: `cpt-cf-approval-service-fr-context-attributes`, `cpt-cf-approval-service-fr-resource-projection`

#### Standing Approvals and Revocation

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-standing-approvals`

Where a Governed Operation allows it, an Approver **MUST** be able to give a Standing Approval, and (p2) limit it by an end date or give it to a group. A stricter Approval Policy **MUST** make existing Standing Approvals insufficient. A revoked approval **MUST** stop allowing operations within the Approval Lease.

- **Rationale**: Ongoing access, such as use of an AI model, needs one approval, not one per use, and must be revocable.
- **Actors**: `cpt-cf-operation-governance-actor-approver`
- **Realized by**: `cpt-cf-approval-service-fr-submit-decision`, `cpt-cf-approval-service-fr-decision-expiry`, `cpt-cf-approval-service-fr-subject-groups`, `cpt-cf-approval-service-fr-approval-lease`

#### Waiting for a Decision

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-wait`

A Consuming Gear **MUST** be notified when an Approval Request is decided or closed, and **MUST** be able to read the result by the request reference. Parked work **MUST** survive a restart of the Consuming Gear. (p2) A Consuming Gear **MUST** be able to escalate a waiting request. Escalation **MUST NOT** approve anything or change the requirement.

- **Rationale**: Long-running work resumes on a Decision without polling, and the Consuming Gear owns its own timers.
- **Actors**: `cpt-cf-operation-governance-actor-consuming-gear`, `cpt-cf-operation-governance-actor-event-broker`
- **Realized by**: `cpt-cf-approval-service-fr-event-notifications`, `cpt-cf-approval-service-fr-request-escalation`

### 5.4 Explanation and Audit

#### Explainable Outcomes

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-explainable-outcome`

Every Governance Outcome **MUST** name what produced it: the Governance Stage and the Policy with its version, the Default, the Decisions with their Approvers and reasons, or, when no Policy applied, the Applicability Snapshot version. Trial Mode results **MUST NOT** be mixed with real outcomes.

- **Rationale**: "Why was this allowed?" must always have an answer.
- **Actors**: `cpt-cf-operation-governance-actor-requester`, `cpt-cf-operation-governance-actor-auditor`
- **Realized by**: `cpt-cf-approval-service-fr-verify-approval`, `cpt-cf-approval-service-fr-correlation`, `cpt-cf-policy-engine-fr-denial-reason`

#### Audit Trail

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-fr-audit-trail`

An Auditor **MUST** be able to follow one operation by its Correlation ID across Authorization, every Governance Stage, and every resumed attempt: the refusal, if any, the Policy versions, every Decision with its reason, and the result. Every Policy change **MUST** be recorded with who made it, when, why, and the content before and after. (p2) An Auditor **MUST** be able to get a report of who may approve and who did approve, per period. (p3) Reviewers **MUST** be able to recertify Standing Approvals by a deadline.

- **Rationale**: Compliance programmes need proof of who allowed what, and of who changed the rules.
- **Actors**: `cpt-cf-operation-governance-actor-auditor`
- **Realized by**: `cpt-cf-approval-service-fr-audit-trail`, `cpt-cf-approval-service-fr-correlation`, `cpt-cf-approval-service-fr-access-review`, `cpt-cf-approval-service-fr-recertification`, `cpt-cf-admission-control-fr-refusal-events`, `cpt-cf-policy-engine-fr-administration-audit`

## 6. Non-Functional Requirements

> **Global baselines**: Project-wide NFRs are defined in the [Architecture Manifest](../../ARCHITECTURE_MANIFEST.md) and the [project guidelines](../../../guidelines/README.md). Only subsystem NFRs are listed here.

### 6.1 Subsystem NFRs

#### Fail Closed

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-nfr-fail-closed`

If a required check is missing, times out, fails, or its gear is not deployed, the operation **MUST NOT** start or resume. If the Applicability Snapshot is missing or older than the Policy Propagation Delay, the Governance Enforcer **MUST** call the Governance Check instead of skipping it. A needed Fact that is still missing after Admission Control asked for it **MUST** give Unavailable.

- **Threshold**: Zero Allowed outcomes on failure paths. The only exception is a cached approval result within its Approval Lease.
- **Rationale**: A governance check that fails open is no check.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

#### No Cost Without Policies

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-nfr-zero-policy-overhead`

When no Policy and no Default other than Allow applies to a Governed Operation for the tenant, governance **MUST** add no I/O, no network call, and no Fact loading to the operation.

- **Threshold**: 0 I/O and 0 network calls; at most 0.1 ms p99 added, measured at the Consuming Gear.
- **Rationale**: Every authenticated operation is governable, but most are not governed.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

#### Check Latency

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-nfr-latency`

A governed operation **MUST** make one Governance Check call per attempt, in addition to Authorization, plus at most one more when Admission Control asks for missing Facts. The Governance Check **MUST** add no more than the sum of: Admission Control overhead of 5 ms p95, the Guardrail Stage bound (100 ms by default), Approval Service verification of 50 ms p95, and the ToolKit out-of-process overhead for each remote hop (5 ms p95 on one host, 10 ms p95 in a Kubernetes cluster).

- **Threshold**: As stated, measured at the Consuming Gear.
- **Rationale**: Checks run on every governed operation.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

#### Bounded Revocation and Propagation

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-nfr-revocation-delay`

A revoked approval or a new deny Decision **MUST** stop new operations within the Approval Lease. A new or changed Policy **MUST** apply within the Policy Propagation Delay.

- **Threshold**: The configured maximum Approval Lease and Policy Propagation Delay.
- **Rationale**: Revoking access and adding a restriction must work quickly and predictably.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

#### Tenant Isolation

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-nfr-tenant-isolation`

Policies, Approval Requests, Decisions, Applicability Snapshots, and audit records of one tenant **MUST NOT** be visible to, or affect, another tenant, except as the tenant tree and [§5.2](#52-configurable-policies) allow.

- **Threshold**: Zero reads across tenants or Self-Managed Tenant barriers in authorization tests.
- **Rationale**: Tenants share one platform.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

#### Sensitive Facts

- [ ] `p1` - **ID**: `cpt-cf-operation-governance-nfr-sensitive-facts`

Fact values and justifications **MUST NOT** appear in logs, traces, or events. Only the Requester, eligible Approvers, and Tenant Administrators of the tenant where the request is decided **MUST** be able to read them. They **MUST NOT** be kept longer than the audit retention period. A Fact declared sensitive **MUST NOT** be sent to an external decision engine unless the Platform Operator allowed it for that engine. Secrets and payment card data **MUST NOT** be declared as Facts.

- **Threshold**: Zero Fact values in logs, traces, and events; zero reads outside these roles in authorization tests.
- **Rationale**: Facts carry business data, such as amounts and vendors.
- **Architecture Allocation**: See DESIGN.md NFR Allocation.

### 6.2 NFR Exclusions

- Availability targets: inherited from the platform and from each gear. The Approval Service availability target is open ([§13](#13-open-questions)).

## 7. Public Library Interfaces

Operation Governance is used through these interfaces. [DESIGN §3.3](./DESIGN.md#33-api-contracts) describes them.

| Interface | Used by | Defined in |
|---|---|---|
| Governance Enforcer, Fact provider, and Governed Operation base type (Admission Control SDK); governance step of operation declarations (ToolKit) | Consuming Gear | [DESIGN](./DESIGN.md), [ADR 0002](./ADR/0002-cpt-cf-operation-governance-adr-fact-collection.md) |
| Governance decision API of Admission Control (`AdmissionClientV2`): Governance Check, batch Preview, Applicability Snapshot | Governance Enforcer | [ADR 0001](./ADR/0001-cpt-cf-operation-governance-adr-governance-gate.md); today's `AdmissionClientV1` in the [Admission Control PRD](../../../gears/system/admission-control/docs/PRD.md#7-public-library-interfaces) |
| Governance Stage plugin contract | Policy Engine, Approval Service | [ADR 0001](./ADR/0001-cpt-cf-operation-governance-adr-governance-gate.md) |
| Authorization request to AuthZ Resolver | Consuming Gear | [Authorization design](../authorization/DESIGN.md) |
| Approval Request read and change events | Consuming Gear | [Approval Service PRD §7](../../../gears/approval-service/docs/PRD.md#7-public-library-interfaces) |
| Policy management API (REST and Rust client) | Tenant Administrator, Platform Operator | [Policy Engine PRD §7](../../../gears/system/policy-engine/docs/PRD.md#7-public-library-interfaces) |
| Approval administration and inbox API (REST) | Tenant Administrator, Approver, Auditor | [Approval Service PRD §7](../../../gears/approval-service/docs/PRD.md#7-public-library-interfaces) |

## 8. Use Cases

[USECASES.md](./USECASES.md) describes the expected behaviour in concrete scenarios, grouped as: gear integration, Policy lifecycle, what Policies can express, requests and Decisions, tenant tree, and audit. Each use case names the requirements of this PRD that it exercises.

## 9. Acceptance Criteria

- [ ] A Tenant Administrator can block an authenticated operation that its Gear Vendor did not prepare for governance, using Envelope Facts only.
- [ ] An operation that no Policy governs makes no governance I/O or network call, and loads no Fact.
- [ ] An operation blocked by a Guardrail Policy and also covered by an Approval Rule that requires approval is Blocked, and no Approval Request is opened.
- [ ] In an admission-only Product, an operation without the Approval Stage runs when admitted; an operation with the Approval Stage gets Unavailable.
- [ ] Every failure or timeout of a required check, and every missing or stale Applicability Snapshot, gives a Governance Check or a refusal, never an unchecked Allowed.
- [ ] Approved work that a new Guardrail Policy blocks does not run when it resumes.
- [ ] A Plan that has one refused operation changes no resource.
- [ ] With unchanged inputs, Policies, and Decisions, a Preview predicts the real outcome and writes no request, audit record, or event.
- [ ] A retried check opens no second Approval Request, a repeated request whose operation completed returns its stored result without a check, and a retry of a pending attempt is checked again.
- [ ] Renaming a route or adding a new API version of an operation leaves its Policies matching and its Facts unchanged; a new Governed Operation version that renames a Fact leaves its Policies matching.
- [ ] A check for a Governed Operation by a gear that does not own it gives Invalid.
- [ ] A needed Fact that the gear does not send, even after Admission Control asks for it, gives Unavailable.
- [ ] A child tenant's looser rule has no effect against a Platform Rule or a parent's rule; a Self-Managed Tenant is governed only by Platform Rules and its own rules.
- [ ] An Auditor can follow one operation by its Correlation ID, and no Fact value appears in logs, traces, or events.

## 10. Dependencies

| Dependency | Role in Operation Governance | Documents | Criticality |
|---|---|---|---|
| Admission Control | Runs every Governance Check over ordered Governance Stages; fails closed; publishes refusal events | [PRD](../../../gears/system/admission-control/docs/PRD.md) · [DESIGN](../../../gears/system/admission-control/docs/DESIGN.md) | p1 |
| Policy Engine | Default Guardrail Stage; stores and evaluates Guardrail Policies | [PRD](../../../gears/system/policy-engine/docs/PRD.md) · [DESIGN](../../../gears/system/policy-engine/docs/DESIGN.md) | p1 |
| Approval Service | Approval Stage; stores Approval Rules, Approval Policies, Approval Requests, and Decisions | [PRD](../../../gears/approval-service/docs/PRD.md) · [DESIGN](../../../gears/approval-service/docs/DESIGN.md) · [Upstream requirements](../../../gears/approval-service/docs/UPSTREAM_REQS.md) | p1 for approvals |
| AuthZ Resolver | Decides Authorization before any Governance Check | [Authorization design](../authorization/DESIGN.md) | p1 |
| Tenant Resolver | Tenant tree and Self-Managed Tenant barriers | [Tenant Model](../authorization/TENANT_MODEL.md) | p1 |
| Account Management | Owns tenants and changes between managed and self-managed | [PRD](../../../gears/system/account-management/docs/PRD.md) | p1 |
| Types Registry | Registers the types of Governed Operations, Governance Stages, resources, and events | [PRD](../../../gears/system/types-registry/docs/PRD.md) | p1 |
| ToolKit out-of-process and contract binding | Remote calls, plugin transport, security context propagation, tracing | [OoP PRD](../toolkit-oop/PRD.md) · [OoP DESIGN](../toolkit-oop/DESIGN.md) · [Contract binding PRD](../toolkit-contract-binding/PRD.md) · [Contract binding DESIGN](../toolkit-contract-binding/DESIGN.md) | p1 |
| ToolKit durable execution | Parked work that survives restarts | [PRD](../../../libs/toolkit-durable-execution/docs/PRD.md) | p2 |
| Event Broker | Delivers refusal, Approval Request, and Applicability Snapshot events | [PRD](../../../gears/system/event-broker/docs/PRD.md) | p1 |
| Resource Group | Group membership for Approvers and Standing Approvals | [PRD](../../../gears/system/resource-group/docs/PRD.md) | p2 |
| Canonical errors | How Consuming Gears report outcomes in their public APIs | [PRD](../errors/PRD.md) · [DESIGN](../errors/DESIGN.md) | p1 |

## 11. Assumptions

- Consuming Gears pass the Requester's security context unchanged to every check.
- Consuming Gears can provide the Facts that their Governed Operations declare, normalized as declared (for example, money in one currency). Admission Control checks the shape of Facts, not their truth: a Consuming Gear is trusted to report them correctly.
- Each Consuming Gear owns the operation and can wait, resume, or cancel it.

## 12. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Admission Control becomes a central dependency of every governed operation | An outage stops governed operations | Fail closed by design; the zero-Policy fast path keeps ungoverned operations independent of it; it scales out as a stateless unit ([DESIGN §3.8](./DESIGN.md#38-deployment-topology)) |
| The Applicability Snapshot is stale | A new Policy applies late | Bounded by the Policy Propagation Delay; a stale snapshot falls back to a full Governance Check |
| Block rules exist in two Governance Stages | Administrators do not know where a block comes from | Every outcome names the stage and the Policy; DESIGN gives guidance on where to write a block |
| A Gear Vendor declares a Fact with a wrong Fact Tier or forgets a Fact | Slow checks, or Policies that cannot be written | Envelope Facts always exist; publishing a Policy that reads an undeclared Fact is refused |
| A costly Tenant Rule slows a shared Consuming Gear for every tenant | Higher latency for other tenants | Rule cost is estimated and bounded when a Policy is published; Rego runs only in the Guardrail Stage, never in Consuming Gears |
| A Consuming Gear skips the check or reports false Facts | An operation runs ungoverned | The governance step is mandatory on every route; the conformance kit and a runtime counter catch governed requests without a check; Envelope Facts come only from the security context |

## 13. Open Questions

1. Who normalizes money amounts: each Consuming Gear or a shared currency service?
2. Does Model Registry give up its own approval state in favour of Approval Service?
3. Do tenant mode changes and contract signatures use Approval Service?
4. Is a second-administrator review of Policy changes needed earlier than p3?
5. What is the availability target of the Approval Stage?
6. What default Policy Propagation Delay should Products use?

## 14. Traceability

- **Design**: [DESIGN.md](./DESIGN.md)
- **ADRs**: [ADR/](./ADR/)
- **Use cases**: [USECASES.md](./USECASES.md)
