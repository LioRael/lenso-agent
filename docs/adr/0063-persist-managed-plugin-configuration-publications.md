# ADR 0063: Persist managed Plugin configuration publications

## Status

Accepted.

## Context

ADR 0061 introduced a Host-side Plugin configuration authority port, but its
only production implementation wrote directly to one local Plugin Root. A
future configuration service and the Console need durable review evidence,
publication history, compare-and-swap fencing, and deterministic recovery from
a process failure between publication intent and Plugin Root materialization.

The Plugin Root remains the only desired state observed by the Host reconciler.
The store must not become a second Plan or Generation execution authority.

## Decision

The Agent Web Host provides an opt-in SQLite configuration authority. The Host
selects it explicitly with an absolute database path and a stable authority
reference. Local Plugin Root authoring remains the default.

The store owns three durable records:

- the current desired Plugin Root revision;
- exact proposal evidence, including base revision, candidate revision,
  proposal digest, Plugin identity, Instance identity, reviewed TOML, review
  result, and publication phase; and
- immutable publication history with the successful revision and timestamp.

A proposal persists review evidence without changing desired state. Publication
uses one operation lease and the following state machine:

1. verify the stored desired revision and reviewed proposal;
2. commit the proposal phase as `materializing`;
3. delegate revision-fenced atomic Plugin Root replacement to
   `LocalPluginRootAuthority`;
4. verify the materialized revision; and
5. advance desired state with compare-and-swap, mark the proposal `published`,
   and append publication history.

Startup completes only one exact interrupted `materializing` intent whose base
and candidate revisions explain the current Plugin Root. If the Root changed
without such evidence, or multiple intents could explain it, startup fails
closed. It never overwrites the Root, discards ambiguous evidence, or falls
back to unmanaged publication.

The proposal digest closes the proposal schema and exact reviewed TOML bytes.
The candidate revision remains semantic, so formatting-only TOML changes do not
create a different desired revision.

## Consequences

- Console configuration survives Host restart with durable CAS and history.
- A later remote adapter can preserve the same proposal/publication protocol
  while replacing SQLite transport and storage.
- Direct out-of-band Plugin Root edits are detected as authority divergence.
- Plugin installation, selection, removal, history browsing, rollback policy,
  distributed coordination, authentication, and multi-tenant routing remain
  separate work.
- Kernel still receives only an immutable resolved Plan and knows nothing about
  configuration storage, HTTP, SQLite, or reconciliation.

## Proof

Tests must prove read-only proposals, exact durable evidence, one CAS winner,
publication history, recovery of one interrupted materialization, rejection of
unrecorded Root changes, HTTP publication through the managed authority, and
recovery after a real Host restart.
