# ADR 0111: Managed Marketplace package publication

Status: Accepted

## Context

The npm Console Agent uses SQLite configuration authority. ADR 0110 initially
admitted package installation only for the built-in local Plugin Root authority;
allowing the same filesystem writes under SQLite would leave its desired revision
stale and fail reconciliation after restart.

## Decision

The Host's SQLite authority owns a durable package publication journal. Marketplace
continues to own reviewed proposals, signed metadata and activation receipts.
Only the built-in local authority and the explicitly wired SQLite authority admit
these installations. Remote and opaque authorities require their own support.

The SQLite operation lease precedes the Plugin Root and Generation fences. Before
publication, the authority reconciles existing work, checks the base revision and
records the exact base/candidate pair. The existing package publisher rechecks the
review under the root fences and atomically installs or exchanges the package
directory. It returns the still-held fence with the verified candidate. SQLite
then advances the desired revision and marks the publication in one transaction,
before the caller registers the installation runtime operation and releases the fence.
Reviewed removal uses the same journal and atomically moves the package into
recoverable trash. Direct unreviewed SQLite filesystem mutation stays disabled.

On interruption, reconciliation holds the root fence and compares the durable
intent with the materialized root. The unchanged base aborts the intent; the exact
candidate completes the desired revision transition. Any other revision or an
ambiguous intent fails closed without deleting or overwriting files. Publication
errors retain recovery evidence. This journal does not grant tool permissions or
turn a desired-state publication into runtime readiness.

The configuration store moves from schema v3 to v4, with additive migration of
existing stores. Profiles remain part of the root revision; selected-profile
validation and Generation resolution continue through the existing Host path.
No Marketplace account, browser-to-Agent connection, or Kernel extension is added.

## Validation

Crash-boundary tests cover an unchanged root, a committed candidate, unexplained
edits, stale bases and migration from v3. The Console tool acceptance exercises
local and SQLite authorities with Process installation, upgrade, failed activation,
restart and removal. Public HTTPS Echo acceptance uses two new OS processes per
authority and an initially empty artifact cache.
