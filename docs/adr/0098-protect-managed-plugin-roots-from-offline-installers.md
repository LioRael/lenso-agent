# ADR-0098: Protect managed Plugin Roots from offline installers

Status: Accepted

## Context

The coding Profile installer writes local authoring files. Running it after a
SQLite authority has recorded its desired revision causes materialized-state
conflicts. Custom database paths and remote authorities cannot be discovered
from the default database filename alone.

## Decision

Before serving a nonlocal configuration authority, the Web Host creates the
presence-only `.lenso/managed-configuration` guard in its Agent Home. It contains
no authority credentials, revision, or configuration. The owning authority
remains the only source of those facts. The guard persists across shutdown.
Offline coding Profile installation checks it before migration or file writes
and fails closed. An existing default `plugin-configuration.sqlite3` also blocks
installation for compatibility with older Homes. Custom or remote Homes from
older versions acquire the guard on their first startup with this version.

There is no automatic deletion, authority adoption, or recovery by rewriting
SQLite history. Install Profiles in a fresh Home before starting a managed
Host, or use the owning authority's supported publication operations. Live
managed Profile import remains unsupported. Named Profile Plugin control
continues to require the built-in local authority.

## Consequences

A stale guard can conservatively reject an offline installer; it never grants
write authority. The file is Host metadata outside the Plugin Root. This does
not make arbitrary external filesystem edits safe or serialize an offline
installer with a concurrently starting Host.

ADR-0099 extends this boundary with an online coding Profile import owned by the
built-in SQLite authority. The offline guard remains in force.
