# Gear Maturity

Maturity defines the guarantees a gear gives its consumers and the rules used to review changes. For gears, the only recorded maturity is `maturity` in `gear.gdl` ([GDL reference](https://github.com/constructorfabric/gearbox/blob/main/docs/gdl.md)): `alpha`, `beta`, `stable`, or `deprecated`. How each level is shown in crate versions is decided in [ADR: crate versioning and maturity](../docs/adrs/releases/0001-crate-versioning-and-maturity.md).

- A gear with `implementation = "design"` omits `maturity` and follows `alpha` rules.
- A `deprecated` gear retains the guarantees of its previous level until removal.
- ToolKit libraries (`libs/**`) have no `gear.gdl`: their version form sets the level (`-alpha.N` alpha, `0.x` beta, `1.0+` stable).

## 1. Rules per level

| Rule | `alpha` | `beta` | `stable` |
|---|---|---|---|
| **Consumer guarantee** | No compatibility promise; specifications may lag behind code. | Usable; breaking API (REST, gRPC, SDK, GTS) changes are documented with a migration path. | Production-ready; stable APIs follow SemVer, with deprecation before removal. |
| **Crate publication and version** | Published as a pre-release `X.Y.Z-alpha.N`; any release may break. Consumers pin it with `=`. | Published `0.x` (first line), or `X.0.0-beta.N` / `-rc.N` (preview of a later major). | Published `1.x` or higher; first promotion uses `1.0.0`. |
| **Specification and SDK review** | High-level steering committee review of boundaries, key P1 scope, and SDK shape. PRD/DESIGN may be incomplete. | Thorough review of PRD, DESIGN, relevant ADRs, and SDK; specifications match code. | Same as `beta`; specifications stay current. |
| **Meaningful PRD change** (adds, removes, or changes an actor, requirement, use case, or public interface; editorial-only changes do not count) | Requires high-level steering committee review. | Requires thorough steering committee review. | Requires thorough steering committee review. |
| **SDK** | SDK shape is reviewed; no public API stability guarantee. | Public types belong in the SDK crate, not database entities, Axum types, or internal enums. Contracts are versioned and tolerate unknown fields/variants where applicable; extension points use GTS ([DESIGN_REVIEW.md](DESIGN_REVIEW.md), C4/C7/C8). | Same as `beta`, with SemVer compatibility for stable SDK APIs. |
| **GTS types and MCP tools** | May use major `v0`; a breaking change resets the type registries that hold the type. | Major `v1` or higher; a `v0` type is renamed to `v1` once, at promotion. A breaking change is the next major, served next to the old one. | Same as `beta`; an unstable API uses a new type, never a changed stable one. |
| **REST paths** | `/{gear}/v1/...`, no promise. | `/{gear}/v1/...`, compatible with the previous release. | `/{gear}/v{N}/...`; unstable APIs only under `/{gear}/v{N}beta/...`. |
| **Wire and persisted-data contracts** (REST, gRPC, events, GTS, MCP, config, database) | No compatibility guarantee. | Existing contract versions stay compatible; breaking contracts get a new version alongside the old one. | Same as `beta`; preserve the stable compatibility promise. |
| **Migrations** | May change; alpha data may be dropped. | Existing migrations are final; subsequent migrations are forward-only. | Same as `beta`. |
| **Breaking API changes** | Allowed in any PR; update all in-repo callers in that PR. The next release is `alpha.(N+1)`. | Bump `0.(x+1).0`, use `feat!` / `BREAKING CHANGE`, and add migration notes. | Deprecate in a minor release before removal in the next major; add migration notes. |
| **Tests** | No minimum coverage; tests encouraged. | Unit, integration, and E2E tests; overall code coverage ≥75%. | Unit, integration, and E2E tests; overall code coverage ≥90%, plus performance and security tests. |
| **Security and tenant isolation** | Full requirements apply and are reviewed. | Full requirements apply and are reviewed. | Full requirements apply and are reviewed. |
| **Ownership** | Owner identified. | Owner listed in `.github/CODEOWNERS`. | Same as `beta`. |
| **Production and operations** | No production-readiness requirement. | Production use is optional. | At least one named consumer runs in production; the promotion PR records evidence and the owner-selected qualification period, with no `feat!` / `BREAKING CHANGE` releases during that period. Limits, timeouts, metrics, alerts, and tested rollback are in place. No known BLOCKER/HIGH findings or critical/major functional, security, or performance defects; other gaps are resolved or accepted in an ADR. |

Steering committee representatives are defined in [MAINTAINERS.md](../MAINTAINERS.md). Specification changes (including meaningful PRD changes) and SDK changes require steering committee review at the depth specified above; other PRs follow [CONTRIBUTING.md](../CONTRIBUTING.md).

A **migration note** must be added to `docs/MIGRATION.md` in the same PR as a breaking change. It names the crate and version step (for example, `## file-storage-sdk 1.4 → 2.0`) and explains who is affected, what changed, how to migrate, and an example.

## 2. Explicit maturity transitions

Allowed progression: `alpha` → `beta` → `stable`; any level → `deprecated`. Versions never go back: a new version is higher than every published one. A `stable` gear previews its next major as `(X+1).0.0-alpha.N`, `-beta.N`, `-rc.N` without changing its own level.

**Every transition is explicit and requires approval by at least one steering committee representative listed in [MAINTAINERS.md](../MAINTAINERS.md).** A version bump alone is not a transition. Before approving, the reviewer must ensure **all rules of the target level in §1** are met, using CI for machine-checkable conditions and PR evidence/review for the rest. Review findings are assessed at the target level (§4).

### New gear → `alpha`

- [ ] Create `gear.gdl` with `maturity = "alpha"` (or omit `maturity` for `implementation = "design"`).
- [ ] Set the first version to `0.1.0-alpha.1`, or `<next minor>-alpha.1` if the crate is already on crates.io.
- [ ] Meet every applicable `alpha` rule in §1, including the high-level review.

### `alpha` → `beta`

- [ ] Set `maturity = "beta"` in `gear.gdl`.
- [ ] Drop the pre-release label (`0.7.0-alpha.4` → `0.7.0`) on every crate of the gear, and rename GTS `v0` types to `v1`.
- [ ] Meet every `beta` rule in §1.

### `beta` → `stable`

- [ ] Set `maturity = "stable"` in `gear.gdl` and set the first stable version to `1.0.0`.
- [ ] Meet every `stable` rule in §1, with production evidence and any accepted gaps recorded in the PR/ADRs.
- [ ] Document any remaining unstable features (§3).

### Any level → `deprecated`

- [ ] Set `maturity = "deprecated"` in `gear.gdl`.
- [ ] Name the replacement and migration path in the gear's `README.md`.
- [ ] Mark published SDK entry points `#[deprecated(note = "...")]` and release deprecations in a minor version where applicable.
- [ ] Preserve the previous level's compatibility rules until removal.

## 3. Unstable features in published gears

A `stable` gear may ship an opt-in experimental feature without weakening its existing stable contracts. `beta` gears may use the same approach.

- **Compile-time gate:** declare `unstable-<feature>` in the gear and SDK Cargo features, and in `gear.gdl` under `cargo_features`. Never enable it by default in the SDK; keep Cargo features additive.
- **Runtime gate:** `features.<feature>.enabled` defaults to `false`; when disabled, the feature registers no routes, clients, tools, consumers, or jobs.
- **Separate contract:** use a distinct SDK trait, `/{gear}/v{N}beta/...` REST path, new GTS type, or MCP tool. Do not modify stable contracts to introduce experimental behavior.
- **Always enforced:** security and tenant isolation; migrations are ungated and additive.
- **Graduation or retirement:** review as a specification/SDK change. On graduation, remove the code/runtime gates in a minor release, but retain the old Cargo feature *name* as a compatibility alias until the next major version. Document retirement similarly.

### Versions: experimental features vs. crate prereleases

The crate version describes its **stable API**. Explicitly opt-in unstable APIs can evolve between minor releases, but stable APIs must remain SemVer-compatible; document this experimental exception. A patch may fix an unstable feature without changing its API.

| Release | Appropriate use |
|---|---|
| `1.5.0` | Publish a new, gated `unstable-bulk-import` feature in an otherwise stable crate. |
| `1.5.1` | Fix a bug without changing the feature's API. |
| `1.6.0` | Evolve, graduate, or retire the unstable API; stable APIs remain compatible. |
| `1.5.0-beta.2` | Test the **entire upcoming `1.5.0` release**, not merely mark one feature unstable. Used during the `1.5.0` prerelease cycle, before the final release. |
| `2.0.0-beta.1` | Preview an upcoming major release that changes stable APIs. |

A consumer deliberately adopting an unstable API keeps the default requirement (`version = "1.5"`) and controls the exact version through `Cargo.lock`; Cargo advises against `~1.5`-style upper bounds. For a prerelease, explicitly request and, where appropriate, pin the prerelease version. Cargo excludes prereleases from ordinary stable version requirements.

Cargo warns that removing a published **feature name** breaks dependency resolution, even if the feature is experimental; retaining it as an inert alias is the recommended compatibility approach. See the official [Cargo Features](https://doc.rust-lang.org/cargo/reference/features.html), [SemVer Compatibility](https://doc.rust-lang.org/cargo/reference/semver.html), and [Dependency Version Requirements](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html) guidance.

## 4. Review severity

[DESIGN_REVIEW.md](DESIGN_REVIEW.md) and code review define BLOCKER, HIGH, MEDIUM, and LOW findings.

| Scope | Severity adjustment |
|---|---|
| `alpha` | Lower by one level; report only resulting BLOCKER/HIGH findings. Ignore compatibility and versioning findings. |
| `beta` | No adjustment. |
| `stable` | Raise by one level for public contracts, persisted data, and compatibility. |
| Unstable feature in `stable` | Treat as `beta`; violating the first four unstable-feature rules is HIGH. |
| Maturity transition PR | Review at the **target** level; each unmet criterion is HIGH. |
| ToolKit library | Raise by one level. |

Never lower security or tenant-isolation findings. If maturity is unknown, apply the stricter level.
