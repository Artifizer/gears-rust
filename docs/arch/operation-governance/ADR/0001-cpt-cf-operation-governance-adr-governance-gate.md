---
description: "Decides that Admission Control becomes the single governance gate: one decision API over an ordered chain of Governance Stage plugins, with Approval Service as the Approval Stage and a Governance Enforcer from the Admission Control SDK in each Consuming Gear."
status: proposed
date: 2026-11-23
---
# Admission Control as the Governance Gate over Ordered Governance Stages


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1. Consuming Gear composes the checks](#option-1-consuming-gear-composes-the-checks)
  - [Option 2. Shared enforcement library composes the checks](#option-2-shared-enforcement-library-composes-the-checks)
  - [Option 3. Governance gate over ordered Governance Stages](#option-3-governance-gate-over-ordered-governance-stages)
  - [Option 4. Approval Service calls Admission Control](#option-4-approval-service-calls-admission-control)
  - [Option 5. One merged governance service](#option-5-one-merged-governance-service)
  - [Option 6. Request interceptor at the API gateway](#option-6-request-interceptor-at-the-api-gateway)
  - [Option 7. Composite engine behind today's admit-or-refuse contract](#option-7-composite-engine-behind-todays-admit-or-refuse-contract)
  - [Option 8. External workflow orchestrator](#option-8-external-workflow-orchestrator)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-operation-governance-adr-governance-gate`

Terms and actors are defined in the [PRD](../PRD.md#14-glossary).

## Context and Problem Statement

A Governed Operation can need Authorization (AuthZ Resolver), Guardrail Policies (Policy Engine), and approval (Approval Service). Today Admission Control is an in-process admit-or-refuse gate over one engine, and Approval Service is a separate verification service. Something must order these checks, combine them into one Governance Outcome, open Approval Requests only for operations that can run, and work the same way in every Consuming Gear and every ToolKit deployment profile. Products built on Gears must also be able to add their own kinds of governance and plug in decision engines that their customers already use. Where should the checks be combined, and through which contract?

## Decision Drivers

- **One consistent decision**: every Consuming Gear gets the same order, combination, outcome, and audit, with no per-gear composition logic.
- **Check before asking**: never open an Approval Request for an operation that Authorization or a Guardrail Policy refuses.
- **Composability**: new kinds of governance (for example change-freeze windows, risk scoring, an external GRC system) and external decision engines (OPA, Cedar) can be added without changing Consuming Gears.
- **Industry standards**: follow the PEP/PDP model of [NIST SP 800-162](https://csrc.nist.gov/pubs/sp/800/162/upd2/final), XACML combining rules and obligations, and the request shape of the [OpenID AuthZEN Authorization API](https://openid.net/specs/authorization-api-1_0.html).
- **Deployment independence**: Products with or without Approval Service, in every ToolKit deployment profile (Embedded, Self-Hosted, Kubernetes Native).
- **Scale and latency**: one decision call per governed attempt, stateless horizontal scaling, and no cost for operations that no Policy governs ([ADR 0002](./0002-cpt-cf-operation-governance-adr-fact-collection.md)).
- **Ownership of execution**: running, parking, resuming, and Plans stay with the gear that runs the operation.
- **Fail closed**: any missing or failing part refuses.

## Considered Options

1. **Consuming Gear composes the checks**: each Consuming Gear calls Admission Control and Approval Service and combines the results.
2. **Shared enforcement library composes the checks**: a ToolKit library in each Consuming Gear calls each service and combines the results.
3. **Governance gate**: Admission Control runs an ordered chain of Governance Stage plugins (Guardrail Stage, Approval Stage, and future stages) behind one decision API; a Governance Enforcer from the Admission Control SDK in each Consuming Gear calls it.
4. **Approval Service calls Admission Control**: Approval Service runs the guardrail check before it opens an Approval Request.
5. **One merged governance service**: one gear stores all Policy kinds and approval state and gives one decision.
6. **Request interceptor at the API gateway**: the gateway runs all checks before the request reaches the gear.
7. **Composite engine behind today's admit-or-refuse contract**: one engine plugin calls Policy Engine and then Approval Service.
8. **External workflow orchestrator**: a workflow engine owns the operation, including approvals, and calls the gear when it may run.

| Option | One decision | Check before asking | Composability | Standards | Deployment | Scale | Execution ownership | **Total (35)** |
|---|---|---|---|---|---|---|---|---|
| 1. Gear composes | 1 | 3 | 1 | 3 | 4 | 4 | 5 | **21** |
| 2. Shared library | 3 | 4 | 2 | 3 | 4 | 4 | 5 | **25** |
| **3. Governance gate** | **5** | **5** | **5** | **5** | **4** | **4** | **5** | **33** |
| 4. Approval Service calls admission | 3 | 5 | 2 | 2 | 2 | 3 | 5 | **22** |
| 5. Merged service | 5 | 5 | 2 | 3 | 2 | 3 | 5 | **25** |
| 6. Gateway interceptor | 3 | 4 | 3 | 3 | 2 | 4 | 1 | **20** |
| 7. Composite engine | 3 | 4 | 1 | 1 | 3 | 3 | 4 | **19** |
| 8. Workflow orchestrator | 4 | 5 | 3 | 3 | 2 | 2 | 1 | **20** |

Scores are 1 (worst) to 5 (best). "Fail closed" is not scored: every option can fail closed.

## Decision Outcome

Chosen option: **Option 3, Admission Control as the governance gate over ordered Governance Stages**, because it is the only option that gives every Consuming Gear one consistent decision and still lets Products add kinds of governance and decision engines without touching Consuming Gears. It applies the pattern of the Kubernetes API server (authentication, authorization, then an ordered admission chain of built-in plugins and webhooks) and of Envoy's external authorization filter. It exposes an AuthZEN-shaped decision API, so external decision engines and clients follow a known standard. Admission Control already owns the parts the gate needs: a fail-closed decision order, GTS-discovered plugins, and refusal events.

Authorization stays outside the gate. It returns query constraints that gears compile into SQL, it already has its own decision point, and every gear calls it first.

### Consequences

- **Admission Control gets a new major contract**, added beside `AdmissionClientV1` as parallel traits, following [contract versioning](../../toolkit-contract-binding/ADR/0007-cpt-cf-binding-adr-contract-versioning.md):
  - An `AdmissionClientV2` contract (remote-capable, with REST and gRPC projections). Its request follows AuthZEN: the subject comes from the security context; the action is the Governed Operation type; the resource carries type, ID, tenant, and Resource Version; the context carries the Facts and the Facts the gear could not load; plus the mode (enforce or Preview), the plan version, and the Correlation ID. It has a batch form for Previews. The response is either the names of missing Facts, at most once per attempt, or the Governance Outcome with its Refusal Reason, what produced it, and obligations: the Approval Request reference, the unmet requirements, and the time to check again (Approval Lease).
  - A `GovernanceStagePluginClientV1` plugin contract (remote-capable), registered through Types Registry like today's `AdmissionEnginePluginClientV1`. Each stage declares its order, whether it has side effects, and whether it supports Previews.
- **Admission Control runs the chain**: it validates the Facts against the Governed Operation type ([ADR 0002](./0002-cpt-cf-operation-governance-adr-fact-collection.md)), resolves the Governance Profile from that type and Product configuration, runs side-effect-free stages first and the Approval Stage last, stops at the first refusal (deny-overrides), maps stage results to one Governance Outcome, publishes no event in a Preview, and accepts the caller's Correlation ID. Its "at most one engine", "in-process only", and "verdict only" constraints are replaced.
- **Policy Engine** implements the Guardrail Stage contract in place of today's engine plugin contract.
- **Approval Service** implements the Approval Stage contract. Its administration, inbox, request read, and change events stay as they are. Consuming Gears no longer call its verification directly.
- **Consuming Gears use the Governance Enforcer** after their own Authorization and idempotency replay: it applies the fast path and Fact rules of [ADR 0002](./0002-cpt-cf-operation-governance-adr-fact-collection.md), calls one Governance Check, maps the outcome to canonical errors, parks work durably, checks again before resuming, and handles Plans.
- **Admission Control is stateless and scales out.** In out-of-process profiles it runs as its own unit with the Guardrail Stage ([DESIGN §3.8](../DESIGN.md#38-deployment-topology)).
- The Admission Control, Policy Engine, and Approval Service PRDs and DESIGNs must be updated to match ([DESIGN §4](../DESIGN.md#4-additional-context)). IRM keeps `AdmissionClientV1` until it moves to the Governance Enforcer.

### Confirmation

- The owners of Admission Control, Policy Engine, Approval Service, and the first Consuming Gear review the `AdmissionClientV2` and `GovernanceStagePluginClientV1` contracts against the [DESIGN master sequence](../DESIGN.md#check-wait-and-run-a-governed-operation).
- A stage conformance test kit checks every Governance Stage: order, side-effect declaration, Preview without writes or events, and fail-closed behaviour.
- Each Consuming Gear verifies the [PRD acceptance criteria](../PRD.md#9-acceptance-criteria), in particular: a Guardrail Policy block opens no Approval Request; a Product without Approval Service gives Unavailable for operations with the Approval Stage; approved work blocked by a new Guardrail Policy does not run when it resumes.

## Pros and Cons of the Options

### Option 1. Consuming Gear composes the checks

- Good, because no gear contract changes.
- Good, because each service fails on its own.
- Bad, because every Consuming Gear implements ordering, outcome mapping, and audit linking, so enforcement drifts between gears.
- Bad, because adding a kind of governance changes every Consuming Gear.

### Option 2. Shared enforcement library composes the checks

- Good, because one tested implementation replaces per-gear composition code.
- Neutral, because the Consuming Gear still makes one call per service, so latency is the same as Option 3.
- Bad, because the library must be upgraded in every Consuming Gear to add a stage or fix the order, and gears on older versions behave differently.
- Bad, because there is no single decision point for external engines, audit, or Previews across stages.

### Option 3. Governance gate over ordered Governance Stages

- Good, because order, combination, outcome mapping, Preview, and audit live in one place and are the same for every Consuming Gear.
- Good, because new stages and external decision engines plug in through Types Registry without changing Consuming Gears.
- Good, because the AuthZEN-shaped API and PEP/PDP split match how the industry builds and integrates policy decision points.
- Good, because the Governance Profile is resolved by the gate from trusted configuration, not by each gear.
- Neutral, because the Approval Stage call goes through the gate, so a remote deployment has one more hop than calling Approval Service directly.
- Bad, because Admission Control becomes a central dependency of every governed operation. The fast path of ADR 0002, stateless scaling, and fail-closed behaviour limit the impact.
- Bad, because it changes the Admission Control contract and its plugin contract, and IRM must migrate.

### Option 4. Approval Service calls Admission Control

- Good, because requests are never opened for blocked operations.
- Bad, because operations without the Approval Stage still call Admission Control directly, so there are two decision paths.
- Bad, because the approval service becomes the entry point for all governance, and admission-only Products would have to deploy it.

### Option 5. One merged governance service

- Good, because there is one Policy store, one place to write a block, and one decision.
- Bad, because it replaces two gears in one large change and ties stateless Guardrail Policies to stateful approval storage.
- Bad, because it closes the door to external decision engines as stages.

### Option 6. Request interceptor at the API gateway

- Good, because external requests cannot skip it.
- Bad, because the gateway lacks the Facts, the Resource Version, and the Plan, and cannot park or resume work.
- Bad, because calls between gears do not pass through the gateway.

### Option 7. Composite engine behind today's admit-or-refuse contract

- Good, because no contract changes.
- Bad, because admit-or-refuse cannot express Approval pending or a request reference; the Requester sees Blocked.
- Bad, because every pending result becomes a refusal event, the engine timeout also bounds the approval call, and Previews remain impossible.

### Option 8. External workflow orchestrator

- Good, because complex human workflows are first-class.
- Bad, because the orchestrator, not the gear, owns the operation, which breaks gear ownership and adds a heavy dependency to every governed operation.
- Bad, because it does not cover synchronous operations that must answer at once.

## More Information

- [Authorization design](../../authorization/DESIGN.md) and its [PDP/PEP ADR](../../authorization/ADR/0001-pdp-pep-authorization-model.md) define the enforcement model this gate extends.
- [ToolKit contract binding](../../toolkit-contract-binding/DESIGN.md) provides the remote-capable contracts and projections; [ToolKit out-of-process design](../../toolkit-oop/DESIGN.md) provides plugin transport and security context propagation.
- Revisit this decision if the gate becomes a measured bottleneck that stateless scaling does not solve.

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

- `cpt-cf-operation-governance-fr-check-order` — the gate runs stages in a fixed order and stops at the first refusal.
- `cpt-cf-operation-governance-fr-governance-outcome` — the gate maps every stage result to one outcome.
- `cpt-cf-operation-governance-fr-governance-profile` — the gate resolves which stages apply from trusted configuration.
- `cpt-cf-operation-governance-fr-preview` — the gate runs stages in Preview mode without writes or events.
- `cpt-cf-operation-governance-fr-audit-trail` — one Correlation ID across all stages.
- `cpt-cf-operation-governance-nfr-fail-closed` — any missing or failing stage refuses.
- `cpt-cf-operation-governance-principle-ordered-stages` — the design principle this decision sets.
- `cpt-cf-operation-governance-seq-check-and-execute` — the normative sequence.
- Gear requirements changed: `cpt-cf-admission-control-fr-admission-interface`, `cpt-cf-admission-control-fr-engine-selection`, `cpt-cf-policy-engine-fr-admission-engine`, `cpt-cf-approval-service-fr-verify-approval`.
