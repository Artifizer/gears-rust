---
description: "Ten illustrative Approval Service policies written in Sketch, a small readable pseudo-language, with their mapping to rules, approval policies, and auto-approval rules and the expected results."
---

# Policy Examples — Approval Service

<!-- toc -->

- [About This Document](#about-this-document)
- [The Sketch Language](#the-sketch-language)
  - [Grammar](#grammar)
  - [Mapping to the Model](#mapping-to-the-model)
  - [Example Tenant Tree](#example-tenant-tree)
- [Examples](#examples)
  - [1. Procurement Spend Bands](#1-procurement-spend-bands)
  - [2. Retired AI Model, Blocked Everywhere](#2-retired-ai-model-blocked-everywhere)
  - [3. Model Use Needs the Owner's Approval](#3-model-use-needs-the-owners-approval)
  - [4. Four-Eyes Refunds](#4-four-eyes-refunds)
  - [5. Staged Order Approval](#5-staged-order-approval)
  - [6. Repository Write Access](#6-repository-write-access)
  - [7. Pull Request Merge](#7-pull-request-merge)
  - [8. Reseller Rules for Managed Customers](#8-reseller-rules-for-managed-customers)
  - [9. Allowed File Types, Trialled First](#9-allowed-file-types-trialled-first)
  - [10. Auto-Approval of Verified MCP Servers](#10-auto-approval-of-verified-mcp-servers)
- [Gaps Found While Writing the Examples](#gaps-found-while-writing-the-examples)

<!-- /toc -->

## About This Document

This document shows **what policies look like**, using ten scenarios from [USECASES.md](../../../docs/arch/operation-governance/USECASES.md). It is illustrative, not normative:

- **Sketch** is an invented notation for reading and discussion. It is not an API, a file format, or a commitment to build an editor.
- Every Sketch construct maps to a field of the real model in [DESIGN.md](./DESIGN.md) §3.1 and the [schemas](../schemas/schemas/). The [mapping table](#mapping-to-the-model) shows how.
- Gear, opportunity, projection, tenant, user, and group names are invented for illustration. They are not registered types.
- If an example disagrees with [PRD.md](./PRD.md) or [DESIGN.md](./DESIGN.md), those documents win.

Results use the USECASES vocabulary, with the verdict outcome from the PRD in brackets: **Allowed** (`not_required` or `approved`), **Blocked** (`denied`), **Approval pending** (`approval_required` or `pending`), **Invalid**, **Unavailable**.

## The Sketch Language

### Grammar

```text
policy "<name>"
  layer    platform | tenant              -- default: tenant
  scope    @<tenant>
  for      <opportunity pattern>
  on       <projection type pattern> | resource <projection id>
  subject  <subject type pattern>         -- p2, optional

  default  <action>                       -- platform default; layer platform only

  <action> [when <condition>] [say "<message>"]
  ...                                     -- action := block | require approval | allow

  approval [when <condition>]             -- may repeat; each block is one approval policy
    stage "<label>"                       -- may repeat (p2); one stage in p1
      need <N> [from <approver>, ...]
      must <approver>, ...
    lease max <duration>
    human only                            -- p2
    exclude <context attribute>, ...      -- p2
    escalate to <approver>, ...           -- p2

  auto-approve [when <condition>]                 -- p2
  no auto-approve below [when <condition>]        -- p2

  trial                                   -- p3
  reason "<text>"
```

- `<approver>` is `user:<id>` or `group:<id>` (groups are p2).
- `<condition>` is a CEL expression over `opportunity`, `subject`, `projection`, and `context` (p2). If it fails to evaluate, it counts as not matching.
- Comments start with `--`. A `-- gear declares:` comment shows opportunity traits that the gear vendor declares. They are not part of the policy.

### Mapping to the Model

A Sketch `policy` is a **named bundle**. All of its rules, approval policies, and auto-approval rules share one header (`layer`, `scope`, `for`, `on`, `subject`). It matches the USECASES meaning of *Policy*: a named rule set that says Block, Require approval, or Allow.

| Sketch | Model | Notes |
|---|---|---|
| `layer platform` | `layer: configuration` | Configuration file (`rules`, `defaults`, `policies`). Binds every tenant, self-managed ones included. Read at startup |
| `layer tenant` | `layer: runtime` | Changed through the API. Binds the scope tenant and its descendants up to a self-managed barrier |
| `scope @t` | `scope_tenant_id` | |
| `for` | `opportunity_pattern` | Prefix `gts.cf.core.approval.opportunity.v1~` omitted |
| `on` | `resource_selector.projection_type_pattern` / `.projection_id` | Prefix `gts.cf.core.approval.projection.v1~` omitted |
| `subject` | `subject_type_pattern` | p2 |
| `default` | Platform default (configuration `defaults`) | Used only when no rule decides; a tenant rule replaces it |
| `block` / `require approval` / `allow` | `rule.v1~` with `action` `deny` / `require_approval` / `not_required` | One rule per line |
| `when` on a rule | Rule condition | p2, `cpt-cf-approval-service-fr-conditional-policies`; see [gaps](#gaps-found-while-writing-the-examples) |
| `say` | Refusal message (OG-UC-23.5) | See [gaps](#gaps-found-while-writing-the-examples) |
| `approval` | `policy.v1~` | Every applicable policy of the tenant and its ancestors must be satisfied (AND) |
| `approval when` | `policy.condition` | p2 |
| `stage` | `stages[]` | Exactly one in p1; up to 8, in order, in p2 |
| `need N from …` | `min_approvals`, `approvers` | Without `from`, every holder of the approve permission counts |
| `must …` | `mandatory_approvers` | Users in p1; in p2 a group is satisfied by one member |
| `lease max` | `max_lease_secs` | Never above the configured `max_lease_secs` (proposed 900 s) |
| `human only` | `forbid_auto_approval: true` | p2 |
| `exclude` | Approver exclusions | p2, `cpt-cf-approval-service-fr-approver-exclusions`; see [gaps](#gaps-found-while-writing-the-examples) |
| `escalate to` | `escalation_recipients` | p2; naming a recipient grants nothing |
| `auto-approve` | `auto_approval_rule.v1~`, `effect: approve` | p2 |
| `no auto-approve below` | `auto_approval_rule.v1~`, `effect: block_descendants` | p2 |
| `trial` | Trial mode | p3, `cpt-cf-approval-service-fr-governance-trial`; see [gaps](#gaps-found-while-writing-the-examples) |
| `reason` | `reason` | Mandatory for every runtime change |

**Rule order** (PRD `cpt-cf-approval-service-fr-rule-layers`), which every example relies on:

1. A platform or ancestor rule that blocks or requires approval wins (block first).
2. Otherwise the tenant's own most specific matching rule decides. On a tie, the stricter action wins.
3. Otherwise a platform or ancestor `allow`.
4. Otherwise the platform default.
5. Otherwise the gear default.

### Example Tenant Tree

```text
@root            provider (Platform Operator)
└── @northwind   reseller, managed
    ├── @acme    managed customer
    └── @globex  self-managed customer   ── barrier: @northwind rules stop here
```

## Examples

### 1. Procurement Spend Bands

POs of 10,000 or more are blocked, POs from 1,000 to 9,999 need a line manager, and smaller POs pass. **Use cases**: OG-UC-05.1, OG-UC-09.1, OG-UC-10.1, OG-UC-10.2, OG-UC-18, OG-UC-23.5.

```text
policy "Procurement spend bands"
  -- gear declares: default_action require_approval, standing_approvals prohibited
  scope  @acme
  for    cf.procurement._.po_release.v1~
  on     cf.procurement._.purchase_order.v1~

  block            when context.amount_eur >= 10000
                   say  "POs of 10,000 or more go through the procurement desk"
  require approval when context.amount_eur >= 1000 && context.amount_eur < 10000
  allow            when context.amount_eur < 1000

  approval
    stage "Line manager"
      need 1 from group:acme-line-managers

  reason "FY26 spend controls, ticket PROC-112"
```

**Maps to** three runtime rules and one runtime policy:

```json
[
  { "kind": "rule", "layer": "runtime", "scope_tenant_id": "<acme>",
    "opportunity_pattern": "gts.cf.core.approval.opportunity.v1~cf.procurement._.po_release.v1~",
    "resource_selector": { "projection_type_pattern": "gts.cf.core.approval.projection.v1~cf.procurement._.purchase_order.v1~" },
    "action": "deny", "condition": "context.amount_eur >= 10000",
    "reason": "FY26 spend controls, ticket PROC-112" },
  { "kind": "rule", "...": "same header",
    "action": "require_approval", "condition": "context.amount_eur >= 1000 && context.amount_eur < 10000" },
  { "kind": "rule", "...": "same header",
    "action": "not_required", "condition": "context.amount_eur < 1000" },
  { "kind": "policy", "layer": "runtime", "scope_tenant_id": "<acme>",
    "opportunity_pattern": "gts.cf.core.approval.opportunity.v1~cf.procurement._.po_release.v1~",
    "resource_selector": { "projection_type_pattern": "gts.cf.core.approval.projection.v1~cf.procurement._.purchase_order.v1~" },
    "stages": [ { "min_approvals": 1, "approvers": [ { "kind": "group", "id": "acme-line-managers" } ] } ],
    "version": 1, "reason": "FY26 spend controls, ticket PROC-112" }
]
```

| Scenario | Result |
|---|---|
| PO of 500 | Allowed by the `allow` rule |
| PO of 1,000 (bound is inclusive) | Approval pending; a request opens for the line managers |
| PO of 5,000, a line manager approves, buyer retries | Allowed |
| Approved PO edited from 5,000 to 6,000 | Approval pending again: the approval is bound to the old facts |
| PO edited to 12,000 | Blocked, with the "procurement desk" message; no request opens |
| `amount_eur` missing | Invalid; never allowed |

### 2. Retired AI Model, Blocked Everywhere

The Platform Operator blocks one retired model for every tenant, without any approval path. **Use cases**: OG-UC-09.1, OG-UC-20.1, OG-UC-20.2, OG-UC-20.3.

```text
policy "Retire legacy-gpt-3"
  layer  platform
  scope  @root
  for    cf.model_registry._.model_use.v1~
  on     resource 7f9c2e4a-0000-4000-8000-00000000a001   -- projection of model "legacy-gpt-3"

  block say "legacy-gpt-3 is retired; use gpt-x instead"

  reason "Model end of life, 2026-10-01"
```

**Maps to** one configuration rule with `projection_id`. It is read-only through the API and changes take effect on restart (`cpt-cf-approval-service-constraint-static-config`).

| Scenario | Result |
|---|---|
| A user in `@acme` calls the model | Blocked, with the retirement message |
| A user in self-managed `@globex` calls the model | Blocked: platform rules cross barriers |
| `@acme` adds a runtime `allow` for the same model | Still Blocked (rule order step 1) |
| The model owner records an approve decision | Still Blocked: decisions never override a block rule |
| A tenant admin lists applicable rules | Sees the rule; cannot edit it |

### 3. Model Use Needs the Owner's Approval

Every tenant needs the owning tenant's approval to use a model. Once given, the approval covers ongoing use. **Use cases**: OG-UC-13, OG-UC-14.1, OG-UC-20.5, OG-UC-21.5.

```text
policy "Model use is approved by the model owner"
  -- gear declares: default_action not_required, standing_approvals allowed,
  --                decision_authority resource_tenant
  layer  platform
  scope  @root
  for    cf.model_registry._.model_use.v1~
  on     cf.model_registry._.model.v1~

  default require approval

  approval
    stage "Model owner"
      need 1
    lease max 15m

  reason "Platform default: no model is usable until its owner approves it"
```

**Maps to** one platform default (configuration `defaults`) and one configuration policy with `max_lease_secs: 900`. Owner-only approval comes from the gear's `decision_authority` trait, not from the policy.

| Scenario | Result |
|---|---|
| `@acme` user calls `gpt-x` (owned by `@root`), nothing decided yet | Approval pending |
| `@root` admin approves `gpt-x` for tenant subject `@acme` (standing) | Allowed for every `@acme` user, with no new requests |
| `@acme` admin tries to approve `gpt-x` | Refused: only the resource tenant may approve |
| `@northwind` admin records a deny for its subtree | Blocked for `@northwind` and `@acme`, even though `@root` approved |
| `@acme` writes its own `allow` for `cf.model_registry._.model_use.v1~` | Allowed: a tenant rule replaces the platform default |
| `@root` admin revokes the approval | Approval pending again within the revocation delay |

### 4. Four-Eyes Refunds

Refunds above 5,000 need Finance. The person who prepared the refund never approves it, and auto-approval is not accepted. **Use cases**: OG-UC-10.1, OG-UC-10.3, OG-UC-12.3, OG-UC-12.4, OG-UC-14.2.

```text
policy "Refunds above 5,000 need Finance"
  -- gear declares: default_action require_approval, standing_approvals prohibited
  scope  @acme
  for    cf.ledger._.refund_post.v1~
  on     cf.ledger._.refund.v1~

  require approval when context.amount_eur > 5000
  allow            when context.amount_eur <= 5000

  approval
    stage "Finance"
      need 1 from group:acme-finance
    human only
    exclude context.preparer_id

  reason "Dual control on refunds, audit finding AF-2026-07"
```

**Maps to** two runtime rules and one policy with `forbid_auto_approval: true`. The gear converts amounts to one currency (`amount_eur`) before checking (OG-UC-10.3).

| Scenario | Result |
|---|---|
| Refund of 5,000 | Allowed (the bound is `> 5000`) |
| Refund of 8,000 | Approval pending for `group:acme-finance` |
| The requester tries to approve their own refund | Refused: separation of duties |
| The preparer named in `preparer_id`, a Finance member, tries to approve | Refused: excluded person |
| Another Finance member approves | Allowed; the next refund of 8,000 needs its own approval |
| An auto-approval rule matches the refund | Ignored; a human approval is still required |

### 5. Staged Order Approval

Orders of 50,000 or more need Finance and then Legal. Orders of 250,000 or more also need the Sales Director. **Use cases**: OG-UC-10.5, OG-UC-11, OG-UC-17.

```text
policy "Order approval by value"
  scope  @northwind
  for    cf.orders_workflow._.order_approve.v1~
  on     cf.orders._.order.v1~

  require approval when context.tcv_eur >= 50000   -- delta TCV for change orders
  allow            when context.tcv_eur <  50000

  approval when context.tcv_eur >= 50000
    stage "Finance"
      need 1 from group:northwind-finance
    stage "Legal"
      need 1 from group:northwind-legal
    escalate to user:head-of-legal

  approval when context.tcv_eur >= 250000
    stage "Sales Director"
      must user:sales-director

  reason "Commercial approval matrix 2026"
```

**Maps to** two runtime rules and two conditional policies. Policy 1 is staged (p2). Policy 2 applies in parallel, and both must be satisfied (AND). For change orders, Orders Workflow sends the **added** value as `tcv_eur` (OG-UC-10.5).

| Scenario | Result |
|---|---|
| Order of 30,000 | Allowed |
| Order of 80,000 | Approval pending; current stage "Finance" |
| Legal approves before Finance | Not counted; Legal must approve again after Finance |
| Finance, then Legal approve | Allowed (verdict `approved`, `request_closed` with the correlation ID) |
| Order of 300,000 | Approval pending until Finance → Legal **and** the Sales Director approve |
| Workflow's 72 h timer fires, it escalates | `request_escalated` names `user:head-of-legal`; still pending, nothing auto-approved |
| Order amended (version 2) | Workflow withdraws version 1, its request closes, and version 2 is checked from scratch |

### 6. Repository Write Access

Writing to a repository needs two maintainers. The approval is standing, so it covers every push until it expires or is revoked. **Use cases**: OG-UC-12.1, OG-UC-12.6, OG-UC-14.1, OG-UC-14.3, OG-UC-14.4, OG-UC-14.5, OG-UC-19.

```text
policy "Repository write needs two maintainers"
  -- gear declares: default_action require_approval, standing_approvals allowed
  scope  @acme
  for    cf.source_control._.repo_write.v1~
  on     cf.source_control._.repository.v1~

  require approval

  approval
    stage "Maintainers"
      need 2 from group:acme-maintainers
    lease max 10m

  reason "Branch protection baseline"
```

**Maps to** one runtime rule and one policy with `max_lease_secs: 600`. A time-limited grant, such as 7 days, is an **expiry on the decision**, set by the approver (p2). It is not a policy field.

| Scenario | Result |
|---|---|
| One maintainer approves user `dev.ana` | Approval pending: 1 of 2 |
| A second maintainer approves | Allowed for every push by `dev.ana`, with no new requests |
| Both approvals were given with a 7-day expiry, and 7 days pass | Approval pending again |
| Maintainers approve group `team-a` (p2) | Every current `team-a` member is covered; a removed member loses it within the membership cache TTL |
| The policy changes to `need 3` | Existing pairs are no longer enough; Approval pending |
| A maintainer revokes their approval | Approval pending again within the revocation delay |

### 7. Pull Request Merge

Every PR needs a reviewer who is not its author. PRs that touch `infra/` also need SRE, and bot PRs need two reviewers. **Use cases**: OG-UC-10.4, OG-UC-12.4, OG-UC-14.2, OG-UC-18.2.

```text
policy "Pull request merge"
  -- gear declares: default_action require_approval, standing_approvals prohibited
  scope  @acme
  for    cf.source_control._.pr_merge.v1~
  on     cf.source_control._.pull_request.v1~

  block            when context.repository == "legacy-billing"
                   say  "legacy-billing is archived; open the PR in billing-v2"
  require approval when context.repository != "legacy-billing"

  approval
    stage "Review"
      need 1 from group:acme-reviewers
    exclude context.pr_author

  approval when context.changed_paths.exists(p, p.startsWith("infra/"))
    stage "SRE"
      must group:acme-sre

  approval when context.author_is_bot
    stage "Two reviewers for bots"
      need 2 from group:acme-reviewers

  reason "Merge policy v4"
```

**Maps to** two runtime rules and three policies. Only the policies whose condition holds apply, and all of them must be satisfied.

| Scenario | Result |
|---|---|
| PR changes `src/` only, one reviewer approves | Allowed |
| The PR author approves their own PR | Refused: excluded person |
| PR changes `infra/vpc.tf`, one reviewer approves | Approval pending: SRE missing |
| A bot PR changes `infra/`, two reviewers and one SRE member approve | Allowed |
| A new commit is pushed after approval | New projection version: the old request closes and the new version is checked from scratch |
| PR targets `legacy-billing` | Blocked, with the archive message |

### 8. Reseller Rules for Managed Customers

A reseller requires approval for large orders from its managed customers. A customer may add stricter rules but cannot loosen the reseller's. **Use cases**: OG-UC-21.1, OG-UC-21.2, OG-UC-21.3, OG-UC-22.1, OG-UC-22.3.

```text
policy "Northwind: large customer orders"
  scope  @northwind
  for    cf.orders_workflow._.order_approve.v1~
  on     cf.orders._.order.v1~

  require approval when context.tcv_eur >= 10000

  approval when context.tcv_eur >= 10000
    stage "Reseller sales ops"
      need 1 from group:northwind-sales-ops

  reason "Reseller credit control"
```

```text
policy "Acme: stricter order threshold"
  scope  @acme
  for    cf.orders_workflow._.order_approve.v1~
  on     cf.orders._.order.v1~

  require approval when context.tcv_eur >= 5000
  allow            when context.tcv_eur <  5000

  approval
    stage "Acme finance"
      need 1 from group:acme-finance

  reason "Acme internal purchasing rule"
```

**Maps to** runtime rules and policies at two tenant levels. These count as one example because the point is how the two levels interact.

| Scenario | Result |
|---|---|
| `@acme` order of 3,000 | Allowed: Acme's own `allow` (step 2) |
| `@acme` order of 7,000 | Approval pending: `group:acme-finance` only (the Northwind policy's condition is false) |
| `@acme` order of 12,000 | Approval pending: `group:northwind-sales-ops` **and** `group:acme-finance` |
| Acme replaces its rules with `allow when context.tcv_eur < 20000` | An order of 12,000 is still pending: the ancestor's `require approval` wins (step 1) |
| `@globex` (self-managed) order of 12,000 | Northwind's policy does not apply; only `@globex` rules and platform rules do |
| `@acme` becomes self-managed | Northwind's rules stop applying from the next check |

### 9. Allowed File Types, Trialled First

The "contracts" bucket accepts only PDF, DOCX, PNG, and JPG. The rule runs in trial mode before it is enforced. **Use cases**: OG-UC-06.2, OG-UC-06.3, OG-UC-09.3, OG-UC-09.4, OG-UC-23.3.

```text
policy "Contracts bucket: document types only"
  -- gear declares: default_action not_required, standing_approvals prohibited
  scope  @acme
  for    cf.file_storage._.file_upload.v1~
  on     cf.file_storage._.bucket.v1~

  block when context.bucket == "contracts"
          && !(context.extension in ["pdf", "docx", "png", "jpg"])
        say "The contracts bucket accepts PDF, DOCX, PNG and JPG only"

  trial
  reason "Contract archive hygiene; trial for two weeks"
```

**Maps to** one runtime rule in trial mode. When the condition is false, the rule does not match and the gear default (`not_required`) applies, so no `allow` line is needed.

| Scenario | Result |
|---|---|
| In trial: upload `scan.exe` to "contracts" | Allowed; admins see "would have been blocked" in the trial report |
| In trial: upload `deal.pdf` | Allowed; nothing reported |
| Publisher removes `trial` | Takes effect from the next upload, with no restart |
| Enforced: upload `scan.exe` to "contracts" | Blocked, with the file-type message |
| Enforced: upload `scan.exe` to "invoices" | Allowed: the condition does not hold |
| `.exe` files already stored in "contracts" | Untouched: rules apply to new operations only |

### 10. Auto-Approval of Verified MCP Servers

The provider auto-approves MCP servers from verified publishers. No tenant below may auto-approve servers that can write files. **Use cases**: OG-UC-14.1, OG-UC-19.2; upstream `cpt-cf-approval-service-upreq-auto-approval`.

```text
policy "MCP servers: verified publishers pass"
  -- gear declares: default_action require_approval, standing_approvals allowed
  scope  @root
  for    cf.mini_chat._.mcp_server_enable.v1~
  on     cf.mini_chat._.mcp_server.v1~

  require approval

  approval
    stage "Security"
      need 1 from group:platform-security

  auto-approve          when context.publisher_verified && context.transport == "https"
  no auto-approve below when "filesystem:write" in context.scopes

  reason "MCP onboarding baseline"
```

**Maps to** one runtime rule, one policy, and two auto-approval rules (`effect: approve` and `effect: block_descendants`). A runtime rule at `@root` binds managed descendants but stops at self-managed barriers. To bind every tenant, write the policy with `layer platform`.

| Scenario | Result |
|---|---|
| `@acme` enables a verified HTTPS server | Allowed by the auto-approval rule; the automatic approval is audited |
| `@acme` enables an unverified server | Approval pending for `group:platform-security` |
| Security approves the unverified server for tenant `@acme` (standing) | Allowed for every `@acme` user |
| `@acme` adds its own `auto-approve` for a server with `filesystem:write` | Ignored: blocked by the `@root` `block_descendants` rule; still pending |
| Security revokes the approval | The server's tools disappear for users within the revocation delay |
| `@globex` (self-managed) enables any server | Not governed by this policy; `@globex` rules, platform rules, and defaults decide |

## Gaps Found While Writing the Examples

These examples rely on behaviour that the PRD or USECASES require but the current schemas do not yet express:

| # | Gap | Source | Used in examples |
|---|---|---|---|
| 1 | Rule conditions: the PRD says rules may carry a condition, but [rule.v1~](../schemas/schemas/gts.cf.core.approval.rule.v1~.schema.json) has no `condition` field | `cpt-cf-approval-service-fr-conditional-policies` | 1, 4, 5, 7, 8, 9 |
| 2 | Block messages: OG-UC-23.5 shows an admin message to the requester; `rule.v1~` has only `reason`, the change reason | OG-UC-23.5 | 1, 2, 7, 9 |
| 3 | Approver exclusions: [policy.v1~](../schemas/schemas/gts.cf.core.approval.policy.v1~.schema.json) has no field naming excluded context attributes; DESIGN marks it "design pending" | `cpt-cf-approval-service-fr-approver-exclusions` | 4, 7 |
| 4 | Trial mode: no field in `rule.v1~` or `policy.v1~`; DESIGN marks it "design pending" | `cpt-cf-approval-service-fr-governance-trial` | 9 |
| 5 | Names: USECASES calls a policy "named" and wants refusals to name it "in business words" (OG-UC-09.1); `rule.v1~` and `policy.v1~` have no name or display-name field | USECASES Shared Vocabulary, OG-UC-09.1 | All |
| 6 | Terminology: in USECASES a *Policy* is the whole rule set (block, require, allow); in DESIGN an *Approval Policy* is only the composition of approvals after `require_approval`. Sketch's `policy` follows USECASES | USECASES vs DESIGN §3.1 | All |
