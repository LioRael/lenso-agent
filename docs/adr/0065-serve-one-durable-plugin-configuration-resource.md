# ADR 0065: Serve one durable Plugin configuration resource

## Status

Accepted.

## Context

ADR 0064 defined and adapted the remote Plugin configuration protocol, but its
proof server existed only inside adapter tests. A deployable service must use
the same durable CAS and publication evidence as local managed configuration,
must stream changes across every Plugin Instance in one resource, and must not
become a remote Plan or Generation authority.

## Decision

The Agent Web package provides the
`lenso-plugin-configuration-service` executable and an embeddable
`PluginConfigurationService` Router. One process serves exactly one explicit
`App / environment` resource backed by:

- a dedicated service Plugin Root;
- an exact compatible Host Catalog at `.lenso/host-catalog.json`; and
- the ADR 0063 SQLite configuration authority.

The SQLite publication ledger is the only change-feed source. Publications are
globally ordered across Plugin IDs and Instance keys by their durable ledger
position. A change request locates its revision or cursor inside SQLite and
returns at most 64 continuous transitions. The service does not load an
arbitrary history window into memory and does not synthesize a whole-root
snapshot when a revision is unavailable.

HTTP requests are bounded to 1 MiB. Change waits are bounded to 30 seconds and
poll durable state at a bounded interval. Responses are bounded to 2 MiB,
change batches shrink without breaking their revision/cursor fence, and the
Router admits at most 128 concurrent requests. SQLite and Plugin Root
operations run off the async request lane. The standalone server starts only
with an absolute UTF-8 service root, a decodable Host Catalog, an absolute
SQLite path, and valid explicit resource identity.

Two distinct bearer credentials provide the first service authorization
boundary:

- the read credential permits inspection, history, and change watching; and
- the write credential permits every read operation plus proposal,
  publication, and rollback proposal operations.

Tokens are supplied through environment variables, are never CLI arguments or
debug output, and are stored in service state only as SHA-256 digests. This is
resource-scoped access control, not a complete tenant/user RBAC system. HTTPS
termination and secret distribution remain deployment responsibilities.

## Consequences

- The remote Host adapter and tests exercise the production Router rather than
  a second test-only protocol implementation.
- Console-capable Hosts use the write credential. Read-only Hosts may watch and
  recover desired configuration but cannot change it.
- The service validates proposed TOML against its compatible Host Catalog, but
  every consuming Host still independently resolves its own Catalog and gates
  its own immutable Generation.
- Multi-resource routing, tenant/user RBAC, approval policy, coordinated
  multi-Host rollout, high availability, and publication retention policy
  remain separate control-plane work.
- Kernel owns no HTTP, SQLite, authorization, Plugin Root, or change-feed
  behavior.

## Proof

Tests must start the real service executable, verify resource hiding and
read/write separation, exercise remote proposal/publication/history/rollback,
recover one global transition chain across Plugin Instances, reject a missing
revision chain without overwriting the local Plugin Root, and switch a real
Host only after the existing Ready Gate accepts the materialized change.
