---
description: "Use cases for Operation Governance: how Policies apply, how a Governed Operation is checked, and how approvals work."
---

# Use Cases — Operation Governance

<!-- toc -->

- [About This Document](#about-this-document)
- [Example Consuming Gears](#example-consuming-gears)
- [A. Gear Integration](#a-gear-integration)
  - [OG-UC-01 Declare a Governed Operation — `p1`](#og-uc-01-declare-a-governed-operation--p1)
  - [OG-UC-02 Check an Operation Before Performing It — `p1`](#og-uc-02-check-an-operation-before-performing-it--p1)
  - [OG-UC-03 Preview a Result Without Effects — `p1`](#og-uc-03-preview-a-result-without-effects--p1)
  - [OG-UC-04 Wait for an Approval Outcome — `p1`](#og-uc-04-wait-for-an-approval-outcome--p1)
- [B. Policy Lifecycle](#b-policy-lifecycle)
  - [OG-UC-05 Write a Policy — `p1`](#og-uc-05-write-a-policy--p1)
  - [OG-UC-06 Test a Policy Before It Takes Effect — `p3`](#og-uc-06-test-a-policy-before-it-takes-effect--p3)
  - [OG-UC-07 Change a Live Policy — `p1`](#og-uc-07-change-a-live-policy--p1)
  - [OG-UC-08 Roll Back or Retire a Policy — `p1`](#og-uc-08-roll-back-or-retire-a-policy--p1)
- [C. What Policies Can Express](#c-what-policies-can-express)
  - [OG-UC-09 Block an Operation — `p1`](#og-uc-09-block-an-operation--p1)
  - [OG-UC-10 Require Approval When a Condition Holds — `p2`](#og-uc-10-require-approval-when-a-condition-holds--p2)
  - [OG-UC-11 Require Ordered Approval Stages — `p2`](#og-uc-11-require-ordered-approval-stages--p2)
  - [OG-UC-12 Decide Who May Approve — `p1`](#og-uc-12-decide-who-may-approve--p1)
  - [OG-UC-13 Let Only the Resource Owner Approve — `p1`](#og-uc-13-let-only-the-resource-owner-approve--p1)
  - [OG-UC-14 Approve Once for Ongoing Use — `p1`](#og-uc-14-approve-once-for-ongoing-use--p1)
- [D. Requests and Decisions](#d-requests-and-decisions)
  - [OG-UC-15 Ask for Approval and Track It — `p1`](#og-uc-15-ask-for-approval-and-track-it--p1)
  - [OG-UC-16 Review and Decide on Requests — `p1`](#og-uc-16-review-and-decide-on-requests--p1)
  - [OG-UC-17 Escalate a Waiting Request — `p2`](#og-uc-17-escalate-a-waiting-request--p2)
  - [OG-UC-18 Keep Approvals Tied to What Was Approved — `p1`](#og-uc-18-keep-approvals-tied-to-what-was-approved--p1)
  - [OG-UC-19 Revoke an Approval — `p1`](#og-uc-19-revoke-an-approval--p1)
- [E. Tenant Tree](#e-tenant-tree)
  - [OG-UC-20 Apply Platform Rules — `p1`](#og-uc-20-apply-platform-rules--p1)
  - [OG-UC-21 Inherit Rules Down the Tenant Tree — `p1`](#og-uc-21-inherit-rules-down-the-tenant-tree--p1)
  - [OG-UC-22 Keep Self-Managed Tenants Independent — `p1`](#og-uc-22-keep-self-managed-tenants-independent--p1)
- [F. Audit and Compliance](#f-audit-and-compliance)
  - [OG-UC-23 Explain Any Result — `p1`](#og-uc-23-explain-any-result--p1)
  - [OG-UC-24 Review Policy Change History — `p1`](#og-uc-24-review-policy-change-history--p1)
  - [OG-UC-25 Review and Recertify Standing Approvals — `p2`](#og-uc-25-review-and-recertify-standing-approvals--p2)
  - [OG-UC-26 Protect Sensitive Facts — `p1`](#og-uc-26-protect-sensitive-facts--p1)
- [Scope and Open Questions](#scope-and-open-questions)
- [Coverage of Existing Gear Requirements](#coverage-of-existing-gear-requirements)

<!-- /toc -->

## About This Document

This document lists concrete scenarios for Operation Governance, what must happen in each, and which scenarios matter most for the rollout. It uses the actors and terms defined in the [PRD](./PRD.md): see [Actors](./PRD.md#2-actors) and [Glossary](./PRD.md#14-glossary). Capitalized terms, such as Governed Operation, Facts, and Approval Rule, have the meaning given there. Priorities (`p1`–`p5`) are defined in the glossary.

Each use case lists:

- **Actor**: a PRD actor.
- **Requirements**: the PRD requirements it exercises.
- **Gear requirements**: the gear requirements that deliver it, where they exist. **Owned by** names behaviour that a gear or a Consuming Gear owns outside Operation Governance.

Expected results use the [Governance Outcome](./PRD.md#governance-outcome) values: Allowed, Blocked, Approval required, Approval pending, Invalid, and Unavailable.

## Example Consuming Gears

The scenarios use these gears as examples of a Consuming Gear.

| Consuming Gear | Example Governed Operations |
|---|---|
| Procurement (example only) | Release a purchase order (PO) |
| Orders Workflow, Order Lifecycle, Order Changes | Approve an order or a change order; amend; cancel |
| Model Registry | Use an AI model; list available models |
| LLM Gateway | Call an AI model |
| Account Management | Create a user; change a tenant between managed and self-managed |
| GitHub Mirror | Merge a pull request (PR); read or write a repository |
| Infrastructure Resource Manager (IRM) | Apply an infrastructure change Plan |
| Task Engine, Serverless Runtime | Run work that may wait for approval |
| Ledger, Subscriptions | Post a refund; backdate a posting; extend a trial; transfer |
| File Storage | Upload a file |
| Mini-Chat | Enable an MCP tool server |
| Quota Enforcement, Contracts | Increase a quota; sign a contract |
| BSS Approvals Inbox | Show Approvers one list of requests across Orders Workflow, Products, Pricing, and Contracts |

## A. Gear Integration

### OG-UC-01 Declare a Governed Operation — `p1`

**Actor**: Gear Vendor.
**Example**: Procurement declares "release a PO" with the Facts amount, currency, vendor, and cost center.
**Goal**: make an operation governable without writing approval logic in the gear.
**Requirements**: `cpt-cf-operation-governance-fr-operation-coverage`, `cpt-cf-operation-governance-fr-governed-operation`, `cpt-cf-operation-governance-fr-fact-loading`, `cpt-cf-operation-governance-fr-policy-layers`, `cpt-cf-operation-governance-nfr-zero-policy-overhead`.
**Gear requirements**: `cpt-cf-approval-service-fr-register-req-type`, `cpt-cf-approval-service-fr-context-attributes`, `cpt-cf-approval-service-fr-subject`. **Owned by** ToolKit and the Admission Control SDK: the governance step of operation declarations and the Governed Operation type ([ADR 0002](./ADR/0002-cpt-cf-operation-governance-adr-fact-collection.md)).

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-01.1 | p1 | Gear Vendor declares a Governed Operation type with Facts (their Fact Tier, and whether each is sensitive, shown to Approvers, or binding) and who an approval belongs to (the user, their tenant, or the Resource Version) | Tenant Administrators can write Policies on these Facts; the Consuming Gear needs no approval code |
| OG-UC-01.2 | p1 | Gear Vendor declares the Operation Default (Allow, Require approval, or Block) | Used only when no rule and no Platform Default decides. Tenant Administrators can see it, start from it, and replace it with a Tenant Rule |
| OG-UC-01.3 | p1 | A new gear version adds an optional Fact | Existing Policies keep working; the new Fact becomes available to Policies |
| OG-UC-01.4 | p1 | A new gear version renames, retypes, or removes a Fact | It needs a new version of the Governed Operation. Policies keep matching it; a renamed Fact stays readable under its old name. A Policy that reads a removed or retyped Fact is reported to its Tenant Administrators, and the operation is Unavailable where it applies until the Policy is fixed. Nothing is silently allowed |
| OG-UC-01.5 | p1 | A Tenant Administrator wants to govern an operation that its Gear Vendor did not prepare (Account Management "create user") | Every authenticated operation is already a Governed Operation; a Policy on its Envelope Facts works with no gear change |
| OG-UC-01.6 | p1 | No Policy governs an operation (most LLM Gateway calls) | The operation loads no Fact and makes no governance call; it costs one in-memory lookup |
| OG-UC-01.7 | p1 | A Policy reads a costly Fact (the PO amount, stored in the database) | The Fact is loaded only for operations and tenants where an applicable Policy reads it |
| OG-UC-01.8 | p1 | Gear Vendor excludes an operation from governance | Allowed only with a recorded reason; the Platform Operator can list excluded operations. An operation that neither names its Governed Operation nor is excluded does not build |
| OG-UC-01.9 | p1 | Gear Vendor renames a route or adds a new API version of the operation | Policies keep matching; the Governed Operation and its Facts are unchanged |

### OG-UC-02 Check an Operation Before Performing It — `p1`

**Actor**: Consuming Gear, on behalf of a Requester.
**Example**: Procurement checks a PO release.
**Goal**: one check sequence gives one clear answer.
**Requirements**: `cpt-cf-operation-governance-fr-check-order`, `cpt-cf-operation-governance-fr-governance-outcome`, `cpt-cf-operation-governance-fr-safe-retries`, `cpt-cf-operation-governance-fr-governance-profile`, `cpt-cf-operation-governance-fr-plan`, `cpt-cf-operation-governance-fr-fact-loading`, `cpt-cf-operation-governance-nfr-fail-closed`.
**Gear requirements**: `cpt-cf-admission-control-fr-decision-order`, `cpt-cf-admission-control-fr-fail-closed`, `cpt-cf-approval-service-fr-verify-approval`, `cpt-cf-approval-service-fr-verdict-resolution`, `cpt-cf-approval-service-fr-create-request`, `cpt-cf-approval-service-nfr-availability`. **Owned by** the Consuming Gear: OG-UC-02.6.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-02.1 | p1 | Consuming Gear checks an operation | Exactly one Governance Outcome |
| OG-UC-02.2 | p1 | A Fact is undeclared or malformed | Invalid. Never Allowed by mistake |
| OG-UC-02.3 | p1 | A Governance Check cannot answer (outage, timeout) | Unavailable; the Consuming Gear does not proceed |
| OG-UC-02.4 | p1 | Consuming Gear sends the same check twice (retry) | Same outcome; no second Approval Request or notification |
| OG-UC-02.5 | p1 | An operation is blocked by one Policy and needs approval under another | Blocked. Nobody is asked to approve something that cannot run |
| OG-UC-02.6 | p2 | IRM checks a Plan touching several resources | The Plan is allowed or refused as a whole; nothing is applied partly |
| OG-UC-02.7 | p1 | A Product without Approval Service checks an operation | Guardrail Policies work. An operation whose Governance Profile includes the Approval Stage gets Unavailable, never Allowed |
| OG-UC-02.8 | p1 | A Fact that a Policy reads is not sent, even after Admission Control asks for it | Unavailable. Never Allowed by mistake |
| OG-UC-02.9 | p1 | Requester repeats a request whose operation completed (same idempotency key) after a Policy change | The stored result is returned; no new check. A retry of a refused or pending attempt is checked again |
| OG-UC-02.10 | p1 | A gear checks a Governed Operation that another gear owns | Invalid |

### OG-UC-03 Preview a Result Without Effects — `p1`

**Actor**: Consuming Gear, on behalf of a Requester.
**Example**: Model Registry shows only the models a user may use; IRM previews a Plan.
**Goal**: know the outcome before acting, without side effects.
**Requirements**: `cpt-cf-operation-governance-fr-preview`.
**Gear requirements**: `cpt-cf-approval-service-fr-verify-approval` (read-only), `cpt-cf-approval-service-fr-batch-verify`; Preview mode of the governance gate ([ADR 0001](./ADR/0001-cpt-cf-operation-governance-adr-governance-gate.md)).

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-03.1 | p1 | Consuming Gear previews one operation to enable or disable a button in its UI | Correct outcome; no Approval Request, no audit record, no event |
| OG-UC-03.2 | p1 | Consuming Gear previews one operation for many resources (a model list page) | One outcome per resource, in one call |
| OG-UC-03.3 | p1 | Preview, then perform with the same Facts | Same outcome, except that Approval required in the Preview becomes Approval pending when performed |

### OG-UC-04 Wait for an Approval Outcome — `p1`

**Actor**: Consuming Gear doing long work.
**Example**: Task Engine pauses a 2-million-record export until an Approver (the data owner) decides.
**Goal**: resume when a Decision is made, without polling, and never on stale checks.
**Requirements**: `cpt-cf-operation-governance-fr-wait`, `cpt-cf-operation-governance-fr-recheck`.
**Gear requirements**: `cpt-cf-approval-service-fr-event-notifications`, `cpt-cf-approval-service-fr-create-request`. **Owned by** the Consuming Gear: OG-UC-04.4.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-04.1 | p1 | Work gets Approval pending | The Consuming Gear is notified when the request is decided or closed |
| OG-UC-04.2 | p1 | Consuming Gear reads the result by request reference | Approved or denied, with reasons |
| OG-UC-04.3 | p1 | The Consuming Gear's own wait times out | It checks again; it keeps waiting if still Approval pending |
| OG-UC-04.4 | p3 | No Decision before the Consuming Gear's deadline (Serverless Runtime onboarding, 5 days) | The Consuming Gear applies its own timeout action (cancel, wait, escalate); Approval Service only reports the request state |
| OG-UC-04.5 | p1 | The request is approved, but a new Guardrail Policy now blocks the operation | The Consuming Gear runs all checks again before it resumes; the outcome is Blocked and the work does not run |

## B. Policy Lifecycle

### OG-UC-05 Write a Policy — `p1`

**Actor**: Tenant Administrator.
**Example**: procurement spend bands — 10,000 or more is blocked; 1,000–9,999 needs approval; below 1,000 is allowed.
**Goal**: express a rule in business terms without writing code.
**Requirements**: `cpt-cf-operation-governance-fr-policy-kinds`, `cpt-cf-operation-governance-fr-policy-lifecycle`, `cpt-cf-operation-governance-fr-fact-loading`.
**Gear requirements**: `cpt-cf-approval-service-fr-approval-rules`, `cpt-cf-approval-service-fr-approval-policies`, `cpt-cf-approval-service-fr-governance-versions`, `cpt-cf-approval-service-fr-conditional-policies`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-05.1 | p1 | Tenant Administrator builds the bands in a form editor (threshold bands on one Fact) | One form holds all bands as Approval Rules: Block, Require approval, Allow |
| OG-UC-05.2 | p2 | Tenant Administrator reopens a saved Policy | It opens in the same form, not as code |
| OG-UC-05.3 | p2 | Tenant Administrator starts from the Operation Default | A rule with that action is created and can be edited |
| OG-UC-05.4 | p2 | Tenant Administrator saves an incomplete or invalid draft | Saved as a draft. It cannot be published until fixed, and every error is listed at once |
| OG-UC-05.5 | p3 | A Tenant Administrator needs a rule the form editor cannot express | An advanced editor is available, with the same checks |
| OG-UC-05.6 | p2 | Two Tenant Administrators edit the same Policy at once | The second save is refused with a clear message |
| OG-UC-05.7 | p1 | Tenant Administrator publishes a Policy that reads a Fact the Governed Operation does not declare | Refused, naming the Fact |

### OG-UC-06 Test a Policy Before It Takes Effect — `p3`

**Actor**: Tenant Administrator.
**Example**: a cloud Tenant Administrator trials "no VMs above 64 vCPU" for two weeks.
**Goal**: see the effect of a Policy before it affects anyone.
**Requirements**: `cpt-cf-operation-governance-fr-trial-mode`.
**Gear requirements**: `cpt-cf-approval-service-fr-governance-trial`, `cpt-cf-policy-engine-fr-non-enforcing-assignment` (Guardrail Policies, available in p1), `cpt-cf-policy-engine-fr-publish-dry-run`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-06.1 | p3 | Tenant Administrator previews a draft with sample Facts | Results shown per sample; Requesters unaffected |
| OG-UC-06.2 | p3 | Tenant Administrator puts the Policy into effect in Trial Mode | Requesters unaffected. Tenant Administrators see what *would have been* blocked or sent for approval |
| OG-UC-06.3 | p3 | Tenant Administrator switches from Trial Mode to enforced | Applies within the Policy Propagation Delay. No restart |

### OG-UC-07 Change a Live Policy — `p1`

**Actor**: Tenant Administrator.
**Example**: procurement changes thresholds or Approvers while POs are waiting.
**Goal**: change rules safely, with a predictable effect on work in progress.
**Requirements**: `cpt-cf-operation-governance-fr-change-effect`, `cpt-cf-operation-governance-fr-policy-lifecycle`.
**Gear requirements**: `cpt-cf-approval-service-fr-governance-versions`, `cpt-cf-approval-service-fr-approval-policies`, `cpt-cf-approval-service-fr-request-reconciliation`, `cpt-cf-approval-service-fr-policy-snapshot`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-07.1 | p1 | Policy made **stricter** (lower threshold, more approvals, new stage) while requests wait | New attempts follow the new Policy. Open requests show the new requirement; Decisions already given still count while the Approver is still eligible |
| OG-UC-07.2 | p1 | Policy made **looser** while requests wait | Requests no longer needed close and leave Approvers' lists within a configured, bounded delay; the next attempt is Allowed |
| OG-UC-07.3 | p1 | **Approvers** changed (Finance → Controllers) while requests wait | Open requests go to the new Approvers; earlier Decisions count only if their Approver is still eligible |
| OG-UC-07.4 | p1 | Operations that already ran under the old Policy | Not affected |
| OG-UC-07.5 | p3 | Tenant requires a second Tenant Administrator to approve Policy changes | The change applies only after another Tenant Administrator approves it; until then the current version stays |

### OG-UC-08 Roll Back or Retire a Policy — `p1`

**Actor**: Tenant Administrator; Platform Operator for emergencies.
**Example**: a new version wrongly blocks every PO release.
**Goal**: undo a bad change or remove a Policy quickly.
**Requirements**: `cpt-cf-operation-governance-fr-policy-lifecycle`.
**Gear requirements**: `cpt-cf-approval-service-fr-governance-versions`, `cpt-cf-approval-service-fr-approval-rules`, `cpt-cf-policy-engine-fr-lifecycle-states`. OG-UC-08.3 is future scope (emergency bypass, [PRD §4.2](./PRD.md#42-out-of-scope)).

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-08.1 | p2 | Tenant Administrator rolls back to the previous version | One action; the previous rules apply within the Policy Propagation Delay; the history shows the rollback |
| OG-UC-08.2 | p1 | Tenant Administrator retires a Policy | Operations follow the remaining Policies; requests that only this Policy demanded close |
| OG-UC-08.3 | p4 | A broken Policy prevents even its own fix | The Platform Operator has an audited emergency path |

## C. What Policies Can Express

### OG-UC-09 Block an Operation — `p1`

**Actor**: Tenant Administrator, Platform Operator.
**Example**: block use of a retired AI model for all tenants; block merges into an archived repository. Blocks that use Facts (file types) come with conditions in p2.
**Goal**: forbid operations on a kind of resource or on one resource, with no approval path.
**Requirements**: `cpt-cf-operation-governance-fr-policy-kinds`, `cpt-cf-operation-governance-fr-policy-layers`, `cpt-cf-operation-governance-fr-refusal-reasons`.
**Gear requirements**: `cpt-cf-policy-engine-fr-denial-precedence`, `cpt-cf-approval-service-fr-approval-rules`, `cpt-cf-approval-service-fr-verdict-resolution`, `cpt-cf-approval-service-fr-conditional-policies`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-09.1 | p1 | Operation matches a Guardrail Policy or a blocking Approval Rule | Blocked, naming the Policy in business words; no Approval Request |
| OG-UC-09.2 | p1 | Operation matches no rule | The Platform Default applies, or else the Operation Default (OG-UC-01.2) |
| OG-UC-09.3 | p2 | A rule combines Facts (File Storage: the "contracts" bucket accepts only PDF, DOCX, PNG, JPG) | Blocks only when every condition holds |
| OG-UC-09.4 | p2 | A new blocking rule is added | Applies to new operations only; existing files are untouched |
| OG-UC-09.5 | p1 | A rule is narrowed to actors (one API client, a group of users) | It applies only when the Requester is one of them; other Requesters of the tenant are unaffected |

### OG-UC-10 Require Approval When a Condition Holds — `p2`

**Actor**: Tenant Administrator.
**Example**: PO release needs a line manager from 1,000; Ledger refunds need Finance above 5,000.
**Goal**: send an operation for approval only when its Facts call for it.
**Requirements**: `cpt-cf-operation-governance-fr-policy-kinds`.
**Gear requirements**: `cpt-cf-approval-service-fr-conditional-policies`, `cpt-cf-approval-service-fr-auto-approval`, `cpt-cf-approval-service-fr-subject-type-selector`, `cpt-cf-approval-service-fr-context-attributes`. **Owned by** the Consuming Gear: normalized Facts in OG-UC-10.3 and OG-UC-10.5.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-10.1 | p2 | Facts below the threshold | Allowed |
| OG-UC-10.2 | p2 | Facts exactly at the threshold | Approval pending (bounds are inclusive as written) |
| OG-UC-10.3 | p2 | Thresholds on money | Compared in one currency; amounts in different currencies are never compared directly |
| OG-UC-10.4 | p2 | Condition on a list or pattern Fact (PR changes under `infra/**`; the author is a bot) | The requirement applies only when the condition holds |
| OG-UC-10.5 | p2 | Change orders judged on the **added** value, not the whole contract | The Consuming Gear sends the added value as a Fact; the Policy uses it |

### OG-UC-11 Require Ordered Approval Stages — `p2`

**Actor**: Tenant Administrator.
**Example**: orders of 50,000 or more need Finance, then Legal; 250,000 or more also need the Sales Director.
**Goal**: require several approvals in a fixed order.
**Requirements**: `cpt-cf-operation-governance-fr-policy-kinds`, `cpt-cf-operation-governance-fr-decisions`.
**Gear requirements**: `cpt-cf-approval-service-fr-staged-policies`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-11.1 | p2 | The first stage approves | Still Approval pending; the next stage is shown as current |
| OG-UC-11.2 | p2 | A later-stage Approver approves before the earlier stage | Not counted until the earlier stage is complete |
| OG-UC-11.3 | p2 | All stages approve | Allowed |
| OG-UC-11.4 | p2 | A stage requires 2 Approvers | Two different people must approve |

### OG-UC-12 Decide Who May Approve — `p1`

**Actor**: Tenant Administrator.
**Example**: repository read is approved by team leads, write only by maintainers; refunds by Finance, never by the preparer.
**Goal**: only the right people can approve, and never their own work.
**Requirements**: `cpt-cf-operation-governance-fr-decisions`.
**Gear requirements**: `cpt-cf-approval-service-fr-approval-policies`, `cpt-cf-approval-service-fr-submit-decision`, `cpt-cf-approval-service-fr-approver-exclusions`, `cpt-cf-approval-service-fr-validate-eligibility`, `cpt-cf-approval-service-fr-group-approvers`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-12.1 | p1 | Approvers are named as users | Only they count; with no list, any user whom Authorization allows to approve this Governed Operation may approve |
| OG-UC-12.2 | p1 | A person outside the list tries to approve | Refused |
| OG-UC-12.3 | p1 | The Requester tries to approve their own request | Refused (Separation of Duties) |
| OG-UC-12.4 | p2 | A person named in the Facts is excluded (PR author, refund preparer) | That person cannot approve |
| OG-UC-12.5 | p1 | Different Approvers per Governed Operation (read vs write) | Each operation uses its own list |
| OG-UC-12.6 | p2 | Approvers are named as groups | Current members of the group count |

### OG-UC-13 Let Only the Resource Owner Approve — `p1`

**Actor**: Gear Vendor (declares owner-only approval), Approver (in the tenant that owns the resource).
**Example**: the tenant that owns an AI model decides which descendant tenants may use it.
**Goal**: approval authority stays with the owner of the resource.
**Requirements**: `cpt-cf-operation-governance-fr-governed-operation`, `cpt-cf-operation-governance-fr-decisions`.
**Gear requirements**: `cpt-cf-approval-service-fr-register-req-type`, `cpt-cf-approval-service-fr-submit-decision`, `cpt-cf-approval-service-fr-verdict-resolution`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-13.1 | p1 | An Approver of the owner tenant approves | The approval applies to the using tenants |
| OG-UC-13.2 | p1 | An Approver of a using tenant tries to approve | Refused |
| OG-UC-13.3 | p1 | An Approver of a using tenant **denies** for its own subtree | That subtree is Blocked, even though the owner approved |

### OG-UC-14 Approve Once for Ongoing Use — `p1`

**Actor**: Approver.
**Example**: the owner approves model "gpt-x" once; a maintainer grants repository write access; a Tenant Administrator enables an MCP server.
**Goal**: one approval covers repeated use, instead of one request per use.
**Requirements**: `cpt-cf-operation-governance-fr-standing-approvals`.
**Gear requirements**: `cpt-cf-approval-service-fr-register-req-type`, `cpt-cf-approval-service-fr-submit-decision`, `cpt-cf-approval-service-fr-decision-expiry`, `cpt-cf-approval-service-fr-subject-groups`, `cpt-cf-approval-service-fr-approval-policies`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-14.1 | p1 | Approver gives a Standing Approval | Repeated operations are Allowed without new requests |
| OG-UC-14.2 | p1 | Governed Operation that does not allow Standing Approvals (PO release, order) | Each Resource Version needs its own approval |
| OG-UC-14.3 | p2 | Standing Approval with an end date (write access for 7 days) | Allowed until the end date, then approval is needed again |
| OG-UC-14.4 | p2 | Standing Approval given to a whole group | Every member is covered, but only while they are in the group |
| OG-UC-14.5 | p1 | An Approval Policy later requires more approvals (2 instead of 1) | Existing single approvals are no longer enough; the next check gives Approval pending |

## D. Requests and Decisions

### OG-UC-15 Ask for Approval and Track It — `p1`

**Actor**: Requester.
**Example**: a buyer releases a PO of 5,000.
**Goal**: know what is needed and follow progress.
**Requirements**: `cpt-cf-operation-governance-fr-approval-requests`.
**Gear requirements**: `cpt-cf-approval-service-fr-create-request`, `cpt-cf-approval-service-fr-verify-approval`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-15.1 | p1 | Requester gets Approval pending | Sees the request reference, who must approve, and the current stage |
| OG-UC-15.2 | p1 | Requester adds a justification | Approvers see it |
| OG-UC-15.3 | p1 | Requester withdraws the request | It closes and leaves Approvers' lists |
| OG-UC-15.4 | p1 | All approvals are given and the Requester retries | Allowed |

### OG-UC-16 Review and Decide on Requests — `p1`

**Actor**: Approver.
**Example**: Finance reviews refunds and orders.
**Goal**: decide quickly with the information needed.
**Requirements**: `cpt-cf-operation-governance-fr-decisions`.
**Gear requirements**: `cpt-cf-approval-service-fr-administration`, `cpt-cf-approval-service-fr-submit-decision`, `cpt-cf-approval-service-fr-bulk-approve`, `cpt-cf-approval-service-fr-on-behalf-decision`. **Owned by** BSS Approvals Inbox: OG-UC-16.5.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-16.1 | p1 | Approver opens their list of requests | Sees only requests they may decide, with Facts, justification, what is required, and which Policy asked |
| OG-UC-16.2 | p1 | Approver approves | Recorded with the time and a required reason |
| OG-UC-16.3 | p1 | Approver denies | A reason is required. The operation is Blocked until the denial is withdrawn, even if others approved |
| OG-UC-16.4 | p1 | Approver changes their mind | They can withdraw their own Decision; a Tenant Administrator can revoke any Decision in their scope |
| OG-UC-16.5 | p3 | Approver works in one list across Orders Workflow, Products, Pricing, and Contracts | Orders, contracts, and price and product approvals appear together |
| OG-UC-16.6 | p1 | Approver decides 40 requests at once | All recorded, or none, with a reason per item |
| OG-UC-16.7 | p2 | Approver decides inside another Consuming Gear's screen (Model Registry, Ledger) | Recorded as that Approver's Decision, made through that gear |

### OG-UC-17 Escalate a Waiting Request — `p2`

**Actor**: Requester or Consuming Gear.
**Example**: Orders Workflow escalates an order after 72 hours without a Legal Decision.
**Goal**: get attention for a stuck request without bypassing approval.
**Requirements**: `cpt-cf-operation-governance-fr-wait`.
**Gear requirements**: `cpt-cf-approval-service-fr-request-escalation`. **Owned by** the Consuming Gear: OG-UC-17.3.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-17.1 | p2 | Request is escalated | An escalation event names the recipients from the Approval Policy; Event Broker delivers it; the request is marked escalated in Approvers' lists |
| OG-UC-17.2 | p2 | After escalation | Nothing is approved automatically; the requirement is unchanged |
| OG-UC-17.3 | p2 | Timing of escalation | Decided by the Consuming Gear's own timer, not by Approval Service |

### OG-UC-18 Keep Approvals Tied to What Was Approved — `p1`

**Actor**: Requester, Approver, Consuming Gear.
**Example**: a PO edited after approval; a PR with new commits; an amended or cancelled order.
**Goal**: an approval never carries over to something different.
**Requirements**: `cpt-cf-operation-governance-fr-approval-binding`, `cpt-cf-operation-governance-fr-approval-requests`.
**Gear requirements**: `cpt-cf-approval-service-fr-context-attributes`, `cpt-cf-approval-service-fr-resource-projection`, `cpt-cf-approval-service-fr-request-reconciliation`, `cpt-cf-approval-service-fr-create-request`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-18.1 | p1 | Facts change after approval (PO amount 5,000 → 6,000) | Approval pending again |
| OG-UC-18.2 | p1 | A new Resource Version appears (new PR commit, order version 2) | The Consuming Gear withdraws the old version, so its request closes; the new version is checked from scratch |
| OG-UC-18.3 | p1 | Facts change so that a blocking Policy now applies (PO edited to 12,000) | Blocked |
| OG-UC-18.4 | p1 | The resource is cancelled or removed while a request is open | The request closes; nothing runs |
| OG-UC-18.5 | p1 | A Fact declared as not binding changes (the owner's file count) | The approval still counts; Approvers saw the value at request time |

### OG-UC-19 Revoke an Approval — `p1`

**Actor**: Approver, Tenant Administrator.
**Example**: a model owner revokes "gpt-x"; a Tenant Administrator withdraws approval of an MCP server.
**Goal**: take back an approval and have it stop working quickly.
**Requirements**: `cpt-cf-operation-governance-fr-standing-approvals`, `cpt-cf-operation-governance-nfr-revocation-delay`.
**Gear requirements**: `cpt-cf-approval-service-nfr-revocation-delay`, `cpt-cf-approval-service-fr-approval-lease`, `cpt-cf-approval-service-fr-submit-decision`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-19.1 | p1 | Approval revoked | Operations are no longer Allowed after the Approval Lease ends |
| OG-UC-19.2 | p2 | Standing Approval revoked for ongoing use (MCP server tools) | The capability disappears for users within the same delay |

## E. Tenant Tree

### OG-UC-20 Apply Platform Rules — `p1`

**Actor**: Platform Operator.
**Example**: "No public IP addresses in regulated tenants", for every tenant.
**Goal**: rules that every tenant must follow, with no exceptions.
**Requirements**: `cpt-cf-operation-governance-fr-policy-layers`, `cpt-cf-operation-governance-fr-self-managed`.
**Gear requirements**: `cpt-cf-approval-service-fr-rule-layers`, `cpt-cf-approval-service-fr-tenant-barriers`, `cpt-cf-approval-service-fr-administration`, `cpt-cf-policy-engine-fr-builtin-policies`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-20.1 | p1 | Any tenant, including a Self-Managed Tenant, performs a matching operation | The Platform Rule applies |
| OG-UC-20.2 | p1 | Any tenant, the Root Tenant included, writes a Tenant Rule that would loosen it | The Platform Rule still wins |
| OG-UC-20.3 | p1 | Tenant Administrators look for the rules that apply to them | They can see Platform Rules but not edit them |
| OG-UC-20.4 | p2 | Platform Operator changes a Platform Rule | Done through a controlled change of Product configuration; tenants are informed |
| OG-UC-20.5 | p1 | Platform Operator sets a Platform Default ("require approval for model use") | Applies to every tenant where no rule decides and replaces the Operation Default; a tenant may replace it with its own Tenant Rule |

### OG-UC-21 Inherit Rules Down the Tenant Tree — `p1`

**Actor**: Tenant Administrator of a Reseller; Tenant Administrator of a Managed Tenant.
**Example**: a Reseller requires approval for orders above 10,000 from its Managed Tenants.
**Goal**: a parent governs its Managed Tenants, and they may only add stricter rules.
**Requirements**: `cpt-cf-operation-governance-fr-policy-layers`.
**Gear requirements**: `cpt-cf-approval-service-fr-tenant-hierarchy`, `cpt-cf-approval-service-fr-verdict-resolution`, `cpt-cf-approval-service-fr-delegable-rules`, `cpt-cf-policy-engine-fr-tenant-assignment`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-21.1 | p1 | Managed Tenant performs a matching operation | The Reseller's rule applies |
| OG-UC-21.2 | p1 | Managed Tenant adds a stricter rule (above 5,000) | Both apply; the stricter one wins |
| OG-UC-21.3 | p1 | Managed Tenant adds a looser rule (allow up to 20,000) | No effect against the Reseller's rule |
| OG-UC-21.4 | p3 | Reseller explicitly lets its children relax one rule | That child's relaxation applies |
| OG-UC-21.5 | p1 | No parent rule exists; a Managed Tenant replaces the Operation Default ("require approval") with its own Allow | Allowed: Defaults are starting points, not parent rules |
| OG-UC-21.6 | p1 | Reseller sets an explicit Allow; the Managed Tenant requires approval | Approval pending: the tenant's own rule decides once no parent blocks or requires approval |

### OG-UC-22 Keep Self-Managed Tenants Independent — `p1`

**Actor**: Tenant Administrator of a Self-Managed Tenant; Tenant Administrator of a Reseller.
**Example**: a customer of a Reseller runs itself.
**Goal**: a parent has no control over, or visibility into, a Self-Managed Tenant.
**Requirements**: `cpt-cf-operation-governance-fr-self-managed`.
**Gear requirements**: `cpt-cf-approval-service-fr-tenant-barriers`, `cpt-cf-policy-engine-fr-inheritance-barriers`. **Owned by** Account Management: the mode change itself.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-22.1 | p1 | Self-Managed Tenant performs an operation | Only its own Tenant Rules and Platform Rules apply |
| OG-UC-22.2 | p1 | A Tenant Administrator of the parent tries to see or decide the child's requests | Refused |
| OG-UC-22.3 | p1 | Managed Tenant becomes self-managed (both agree) | Parent rules stop applying within the Policy Propagation Delay |
| OG-UC-22.4 | p1 | Self-Managed Tenant becomes managed again | Parent rules apply again within the Policy Propagation Delay |

## F. Audit and Compliance

### OG-UC-23 Explain Any Result — `p1`

**Actor**: Requester, Approver, Auditor.
**Example**: "Why was PO-4711 released?"
**Goal**: every outcome can be traced to its cause.
**Requirements**: `cpt-cf-operation-governance-fr-explainable-outcome`, `cpt-cf-operation-governance-fr-refusal-reasons`, `cpt-cf-operation-governance-fr-audit-trail`, `cpt-cf-operation-governance-nfr-sensitive-facts`.
**Gear requirements**: `cpt-cf-approval-service-fr-verify-approval`, `cpt-cf-approval-service-fr-correlation`, `cpt-cf-approval-service-fr-refusal-reasons`, `cpt-cf-approval-service-fr-administration`, `cpt-cf-approval-service-fr-governance-trial`, `cpt-cf-admission-control-fr-refusal-events`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-23.1 | p1 | Any outcome | Names the Policy and version, the Default, or the Decisions that produced it |
| OG-UC-23.2 | p1 | Auditor follows one operation by its Correlation ID | One trail: the attempt, the Policy versions, every Decision with reasons, the final outcome |
| OG-UC-23.3 | p3 | Trial Mode results | Reported apart from real refusals, never mixed with them |
| OG-UC-23.4 | p1 | An operation is refused | The outcome carries its Refusal Reason and a readable message |
| OG-UC-23.5 | p1 | Tenant Administrator attaches a message to a blocking Policy ("POs above 10,000 go through procurement") | The Requester sees that message when the Policy blocks them |
| OG-UC-23.6 | p1 | Someone investigates a refusal | Every refusal is logged with its Refusal Reason, code, and Correlation ID, without Fact values; denials always carry the Approver's reason |

### OG-UC-24 Review Policy Change History — `p1`

**Actor**: Auditor, Tenant Administrator.
**Example**: "Which spend Policy changes happened in March, and why?"
**Goal**: every Policy change is attributable.
**Requirements**: `cpt-cf-operation-governance-fr-policy-lifecycle`, `cpt-cf-operation-governance-fr-audit-trail`.
**Gear requirements**: `cpt-cf-approval-service-fr-governance-versions`, `cpt-cf-approval-service-fr-audit-trail`, `cpt-cf-policy-engine-fr-administration-audit`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-24.1 | p1 | Open a Policy's history | Every change with who made it, when, why, and the content before and after |
| OG-UC-24.2 | p2 | Compare two versions | A readable difference |
| OG-UC-24.3 | p2 | Rollbacks and retirements | Shown in the same history |

### OG-UC-25 Review and Recertify Standing Approvals — `p2`

**Actor**: Auditor, Approver.
**Example**: quarterly review of repository write access and refund Approvers.
**Goal**: remove access nobody needs any more.
**Requirements**: `cpt-cf-operation-governance-fr-audit-trail`.
**Gear requirements**: `cpt-cf-approval-service-fr-access-review`, `cpt-cf-approval-service-fr-recertification`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-25.1 | p2 | Auditor asks who may approve and who did, per period | An access review report |
| OG-UC-25.2 | p3 | Approvers confirm or revoke Standing Approvals by a deadline | Revoked approvals stop working (OG-UC-19) |

### OG-UC-26 Protect Sensitive Facts — `p1`

**Actor**: Auditor, Requester.
**Example**: order amounts and justifications.
**Goal**: sensitive Facts are seen only by people who need them.
**Requirements**: `cpt-cf-operation-governance-nfr-sensitive-facts`.
**Gear requirements**: `cpt-cf-approval-service-nfr-data-classification`, `cpt-cf-approval-service-fr-refusal-reasons`.

| ID | Pri | Scenario | Expected result |
|---|---|---|---|
| OG-UC-26.1 | p1 | Someone views a request | Facts and justification are shown only to the Requester, eligible Approvers, and Tenant Administrators of the tenant where the request is decided |
| OG-UC-26.2 | p1 | Logs and events | Never contain Fact values |

## Scope and Open Questions

What Operation Governance does not cover is listed in [PRD §4.2](./PRD.md#42-out-of-scope). Open product questions are tracked in [PRD §13](./PRD.md#13-open-questions).

## Coverage of Existing Gear Requirements

| Source | Requirement or use case | Use cases |
|---|---|---|
| Approval Service PRD | PO release, order escalation, model access, PR merge, repository access, task wait, gear gates an operation | OG-UC-02, OG-UC-04, OG-UC-10–OG-UC-18 |
| Approval Service PRD | Staged and conditional approvals, on-behalf Decisions, groups, expiry, access review, recertification, delegable rules | OG-UC-11, OG-UC-14, OG-UC-16, OG-UC-21, OG-UC-25 |
| Approval Service upstream requirements | Model Registry, Task Engine wait, Orders Workflow requests and escalation, BSS Approvals Inbox, thresholds, bulk Decisions, audit, closing on a new version | OG-UC-04, OG-UC-10, OG-UC-13, OG-UC-16–OG-UC-18, OG-UC-23 |
| Policy Engine PRD | Authoring, activation, built-in policies, Trial Mode (shadow mode), barriers, dormant assignments | OG-UC-05, OG-UC-06, OG-UC-08, OG-UC-20–OG-UC-22 |
| Admission Control PRD | Admission of an operation, fail closed, refusal events | OG-UC-02, OG-UC-23 |
| IRM | Plan admission, Preview parity | OG-UC-02.6, OG-UC-03.3 |
| Orders Workflow, Order Lifecycle, Order Changes | Approval step, escalation, amendment, cancellation, judgement on added value | OG-UC-10.5, OG-UC-11, OG-UC-17, OG-UC-18 |
| BSS Approvals Inbox, Products, Pricing, Contracts | One list of requests; their own approval units; contract signature | OG-UC-10, OG-UC-16.5 |
| Ledger, Subscriptions | Dual control, backdating, Separation of Duties; trial extension; transfer | OG-UC-09, OG-UC-10, OG-UC-12 |
| Model Registry | Model approval by the owner; model lists | OG-UC-03.2, OG-UC-13, OG-UC-14, OG-UC-19 |
| GitHub Mirror | Repository- and path-specific approval rules | OG-UC-10.4, OG-UC-12 |
| Task Engine, Serverless Runtime | Waiting for Decisions; onboarding approvals with a timeout; workflow change review | OG-UC-04, OG-UC-12.3 |
| Account Management | Managed and self-managed modes; mode change | OG-UC-21, OG-UC-22 |
| File Storage, Mini-Chat, Quota Enforcement | File-type Policy; MCP server approval; quota increases | OG-UC-09, OG-UC-10, OG-UC-14, OG-UC-19 |
