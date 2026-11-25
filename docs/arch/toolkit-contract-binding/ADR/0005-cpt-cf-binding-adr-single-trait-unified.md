---
status: rejected
date: 2026-04-10
---

# Single Unified Contract Trait (rejected — two-layer design retained)


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Base trait with separate transport projections](#base-trait-with-separate-transport-projections)
  - [Single unified trait](#single-unified-trait)
- [Status](#status)
- [More Information](#more-information)

<!-- /toc -->

**ID**: `cpt-cf-binding-adr-single-trait-unified`

## Context and Problem Statement

This ADR slot was reserved to evaluate collapsing the **base contract trait** and its
**transport projection** into a single unified trait carrying both the domain contract and the
transport annotations (e.g. `#[toolkit_contract(binding = [compile, rest])]`).

## Decision Drivers

- Keep the domain contract usable by in-process consumers and independently of transport metadata.
- Allow new transports without altering the base contract trait.

## Considered Options

- Retain a base trait with separate transport projections.
- Combine the base trait and transport projections in one trait.

## Decision Outcome

**Rejected.** The two-layer design (a plain base trait + a separate `*Rest`/`*Grpc` projection that
extends it) is retained, as decided in **ADR-0001** (`cpt-cf-binding-adr-contract-source-of-truth`)
and extended in **ADR-0003** (`cpt-cf-binding-adr-projection-server-gen`).

### Consequences

The base trait continues to hold the domain contract. REST and gRPC retain separate projections.

### Confirmation

Review the generated contracts and verify that transport annotations remain on their respective projections and that an in-process consumer can use the base trait independently.

## Pros and Cons of the Options

### Base trait with separate transport projections

- Lets transports evolve independently of the base trait. Each transport needs a separate projection that stays aligned with the base trait.

### Single unified trait

- Gives authors one trait to maintain.

A single unified trait was rejected for the reasons recorded in ADR-0001's alternatives table and in
ADR-0003 Option B:

- it mixes transport concerns into the domain interface, forcing compile-time plugins to carry
  annotation weight for transports they do not use;
- REST annotations do not apply to gRPC, so a unified trait accumulates a stack of per-transport
  attributes on every method as transports are added;
- adding a new transport would require modifying the base trait rather than being purely additive.

## Status

No separate mechanism is introduced by this ADR. It exists only to record that the single-trait
alternative was considered and not adopted; see ADR-0001 and ADR-0003 for the governing decisions.

## More Information

- ADR-0001 — contract source of truth: [`./0001-cpt-cf-binding-adr-contract-source-of-truth.md`](./0001-cpt-cf-binding-adr-contract-source-of-truth.md)
- ADR-0003 — projection server generation (Option B "collapse onto base" rejected): [`./0003-cpt-cf-binding-adr-projection-server-gen.md`](./0003-cpt-cf-binding-adr-projection-server-gen.md)
