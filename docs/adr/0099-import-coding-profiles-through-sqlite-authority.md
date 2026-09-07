# ADR-0099: Import coding Profiles through SQLite authority

Status: Accepted

## Context

ADR-0098 deliberately blocks offline installers once a Host adopts a managed
configuration authority. The official coding preset changes multiple Plugin
configurations, disabled markers, and named Profile documents. Individual
configuration publications cannot express that installation as one recoverable
operation.

## Decision

Agent Web exposes a narrow, Host-authorized coding preset import for its built-in
SQLite authority. The request supplies the current Plugin Root revision and
process stream identity. The server supplies the reviewed preset bytes; clients
cannot upload paths, arbitrary files, or executable packages through this seam.

The SQLite authority owns a `profile_imports` journal containing exact previous
and candidate file bytes, base and candidate revisions, and publication phase.
Its existing operation lease serializes imports with configuration publications.
The Plugin Root authoring lock and Generation mutation fence cover staging,
validation, materialization, and the durable desired-revision CAS. A complete
staged Home must resolve Normal and every imported Profile before publication.
Customized files, symbolic links, oversized files, and stale revisions are
rejected. Existing enabled Instances retain their enabled state.

An interrupted, uncommitted import is undone from its exact journal before
ordinary authority inspection or startup reconciliation. Every affected file
must match either its recorded previous or candidate bytes. Unexplained edits
and ambiguous intents fail closed. A committed import remains available after
restart. Downgrades must not occur with an unfinished import journal.

Import publishes available Profile definitions; it does not claim activation.
Selecting an imported Profile remains a separate Generation Ready Gate operation.
A failed selection retains the old Profile and Generation, and existing Turns
keep their original leases. SQLite checks the selected Profile document against
its import record. Plugin configuration remains governed by the existing managed
Plugin Root revision, including when a Profile is selected.

Only the built-in SQLite adapter advertises `profileImport`. It and the built-in
local authority advertise `profileSelection`; remote and injected authorities do
not inherit these capabilities by declaring a matching source kind. The offline
guard remains unchanged. Arbitrary named Profile uploads, remote authority
imports, and package installation are outside this operation.

## Proof

Tests cover online import and selection, stale stream/revision rejection,
idempotency, preservation of customized files and enabled Instances, exact
interrupted-import recovery, unexplained-edit rejection, and restart with an
imported Profile.
