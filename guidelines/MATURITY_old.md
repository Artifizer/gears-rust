# Gear Maturity

A gear's maturity level sets what its consumers can rely on. It also sets which rules apply to
a pull request that changes the gear: who approves it, how breaking changes are versioned, and
how severe review findings are.

The level is the `maturity` field in the gear's `gear.gdl`: `unpublished`, `preview`, `stable`,
or `deprecated` ([GDL reference](https://github.com/constructorfabric/gearbox/blob/main/docs/gdl.md)).
`gear.gdl` is the only place that records it.

- A gear with `implementation = "design"` has no `maturity` and follows the `unpublished` rules.
- A `deprecated` gear keeps the rules of its level before deprecation until it is removed.
- ToolKit libraries (`libs/**`) have no `gear.gdl`: `0.x` follows `preview`, `>= 1.0` follows
  `stable`.

---

## 1. Rules per level

| | `unpublished` | `preview` | `stable` |
|---|---|---|---|
| **Promise** | None. Anything can change in any release. | Usable. Breaking changes are announced and have a migration path. | Production use. SemVer, and deprecation before removal. |
| **Crate version** | `publish = false`, or `0.x` | `0.x` | `>= 1.0` |
| **Spec and SDK review** | Steering committee (*) approvals required, but in relaxed mode. A high-level review of gear boundaries, key features, and SDK shape. Only BLOCKER and HIGH findings are raised. PRD and DESIGN may be short and lag behind the code. | Steering committee approvals required after careful review. Every meaningful (**) PRD change and every SDK change is reviewed. Specifications match the code. | The same as `preview` |
| **Breaking change** | Allowed in any PR. The same PR fixes every caller in this repository. A published crate bumps `0.(x+1).0`. | `0.(x+1).0`, `feat!`, add migration notes (***). | Next major only. After a deprecation use minor update (`1.x`). Add migration notes. |
| **Wire contracts and data** (REST, gRPC, events, GTS, MCP, config, database) | No promise. GTS types may use major `v0`. | Compatible with the previous release. Migrations are forward-only. GTS types use `v1` or higher. A breaking change is a new version served next to the old one | The same as `preview`. |
| **Tests** | No strict requirements. | There are unit, integration and e2e test with overall code coverage at least 75% | There are unit, integration and e2e test with overall code coverage at least 90%. Additional performance and security tests are also conducted. |
| **Security and tenant isolation** | Full rules. | Full rules. | Full rules. |

(*) The **steering committee** is the maintainers listed in [MAINTAINERS.md](../MAINTAINERS.md). It
approves every specification (PRD, DESIGN, ADR) and SDK change of a gear, at every level. Other
changes follow the normal review of [CONTRIBUTING.md](../CONTRIBUTING.md).

(**) A **meaningful PRD change** adds, removes, or changes an actor, a requirement, a use case, or a
public interface. Wording, formatting, and typo fixes are not meaningful.

(***)A **migration note** is a section in the gear's `docs/MIGRATION.md`, added in the same pull request
as the change. It names the crate and the version step (`## file-storage-sdk 1.4 → 2.0`) and says,
for each change, who is affected, what changed, and what to do, with an example.

---

## 2. Level transitons

A gear moves only up: `unpublished` → `preview` → `stable`, and from any level to `deprecated`.
A published crate is never unpublished, and `1.x` never returns to `0.x`.

Every level change, including a new gear, is approved by the steering committee and three teams from [MAINTAINERS.md](../MAINTAINERS.md)

### `unpublished`

- Every new gear starts here, with `publish = false` on every crate.
- Steering committee reviews the PRD, DESIGN, and SDK at a high level: gear boundaries and key p1 scope. Deep review is not necessary.
- Any change may break earlier behavior. Specifications and code may diverge.
- No requirements for tests and coverage.

### `unpublished` -> `preview`

A pull request sets `maturity = "preview"`, removes `publish = false`, and sets the version to `0.1.0`. It is a formal acceptance:

1. **Specifications.** PRD and DESIGN are final for the current scope and consistent
   with the code. The gear owner and maintainers from two other groups approve them.
2. **Contracts.** Public types live in the SDK crate: no database entities, Axum types, or
   internal enums. Contracts are versioned, tolerate unknown fields and enum variants
   (`#[non_exhaustive]`, `#[serde(other)]`), and extension points use GTS
   ([DESIGN_REVIEW.md](DESIGN_REVIEW.md) C4, C7, C8).
3. **Data.** Migrations are final. From now on they are forward-only.
4. **Security.** Security and tenant isolation are reviewed (C5, C12, C14).
5. **Tests.** See `tests` requirements in §1.
6. **Code.** Code must be inspected by at least one steering committee representative.

### `preview` -> `stable`

A pull request sets `maturity = "stable"` and the version to `1.0.0`. It is a formal acceptance:

1. **Production evidence.** At least one named consumer runs the gear in production. The gear
   owner decides how long and over how many releases, and records the evidence in the promotion
   pull request.
2. **Contract stability.** No `feat!` or `BREAKING CHANGE` during that period.
3. **Quality.** No known critical or major functional, security, or performance bug. No open
   CRITICAL or HIGH review finding. Each known gap is closed or accepted in an ADR.
4. **Operations.** Limits, timeouts, metrics, alerts, and a tested rollback to the previous release
   are in place.
5. **Specifications.** Full PRD, DESIGN, and ADRs. Every remaining unstable feature is listed in
   the card with its stage (§4).
6. **Tests.** The `stable` row of §2 is met.
7. **Approval.** The gear owner and two maintainers from other groups, one of them an architect.

### `* -> deprecated`

A pull request sets `maturity = "deprecated"`, names the replacement and the migration path in the
card, and marks the SDK entry points `#[deprecated(note = "...")]` in a minor release. Contracts
keep working as before. Removal follows the deprecation policy of the level the gear had.


| Transition | The pull request |
|---|---|
| new gear | Starts as `unpublished`. |
| `unpublished` → `preview` | Sets the version to `0.1.0` and removes `publish = false`. * PRD and DESIGN match the code. Public types live in the SDK crate. Migrations are final. Security and tenant isolation are reviewed. The gear owner is in `.github/CODEOWNERS`. |
| `preview` → `stable` | Sets the version to `1.0.0`. At least one consumer runs the gear in production. There are no known BLOCKER or HIGH security or functional issues. Limits, timeouts, metrics, and rollback are in place. |
| any → `deprecated` | Names the replacement in the gear's `README.md` and marks the SDK entry points `#[deprecated]`. |

---

## 3. New features in a `stable` gear

A new feature of a `stable` gear that is not proven yet ships as an **unstable feature**, so the
stable promise holds for everyone who does not turn it on.

1. **Compile-time gate.** A Cargo feature `unstable-<feature>` on the gear crate and its SDK
   crate, listed in `cargo_features` in `gear.gdl`. It is never a default feature of the SDK crate.
2. **Runtime gate.** The configuration flag `features.<feature>.enabled`, default `false`. While it
   is `false`, the feature registers nothing: no route, client, tool, type, consumer, or job.
3. **Own contract.** A separate SDK trait, a `/{gear}/v{N}beta/...` REST path, a new GTS type or
   MCP tool. Never change a stable surface for an unstable feature.
4. **Never gated.** Migrations always run and are additive. Security and tenant isolation apply
   from the first commit.
5. **Graduate or remove.** Graduation removes both gates in a minor release and is reviewed as a
   specification and SDK change.

Unstable items are exempt from SemVer. `preview` gears may use the same mechanism.

### Versioning a `stable` crate with unstable features

This follows common Rust practice: clap ships `unstable-*` Cargo features and tokio ships its
`tokio_unstable` APIs inside `1.x`, and both may break those APIs in a minor release. The rules:

- The version describes only the stable API. An unstable change never bumps the major.
- Any change to an unstable feature (add, break, graduate, remove) is at least a minor release.
  A fix that does not change its API may be a patch.
- Pre-release versions (`2.0.0-beta.1`) are only for previews of the next major release. They are
  not used for unstable features. Cargo never selects a pre-release unless it is named.

Example: `file-storage-sdk` at `1.3.0` adds the unstable feature `bulk-import`.

| Version | Change | Why |
|---|---|---|
| `1.4.0` | Adds `unstable-bulk-import` with a new `BulkImportUnstableApi` trait. | New feature: minor. |
| `1.4.1` | Fixes a bug in bulk import without changing its API. | Patch. |
| `1.5.0` | Renames a method of `BulkImportUnstableApi`. | Breaking, but unstable: minor, not major. |
| `1.6.0` | Graduates: removes the gates, and the trait becomes `BulkImportApi`. | Stable API grows: minor. |
| `1.7.0` | Removes another unstable feature that did not graduate. | Unstable: minor. |
| `2.0.0-beta.1` | Preview of a breaking change to the stable `FileStorageApi`. | Pre-release of the next major. |
| `2.0.0` | Releases that breaking change, after a deprecation in `1.x`. | Breaking stable change: major. |

A consumer of an unstable feature pins the minor version, because the next minor may break it:

```toml
file-storage-sdk = { version = "~1.5", features = ["unstable-bulk-import"] }
```

---

## 4. Review severity

[DESIGN_REVIEW.md](DESIGN_REVIEW.md) and code review define the finding levels (BLOCKER, HIGH,
MEDIUM, LOW). Adjust each finding by the maturity of what it is in:

| Finding is in | Adjustment |
|---|---|
| an `unpublished` gear | One level lower; only BLOCKER and HIGH are raised. Compatibility and versioning findings are not reported. |
| a `preview` gear | None. |
| a `stable` gear | One level higher for public contracts, persisted data, and compatibility. |
| an unstable feature of a `stable` gear | As `preview`. A violation of §3 rules 1–4 is HIGH. |
| a pull request that changes `maturity` | As the target level. Each unmet criterion of §2 is HIGH. |
| a ToolKit library | One level higher. |

Security and tenant isolation findings are never lowered. When the level is unknown, use the
stricter row.

---

## 5. Prerequisites

- Gearbox accepts `unpublished` as a `maturity` value (it replaces `experimental`), and its
  scaffold writes it for a new gear.
- `cargo-gears` lint DE0801 accepts the REST version segment `v{N}beta`.
