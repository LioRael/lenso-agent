# ADR 0013: Inspect Plugin and Generation provenance

## Status

Accepted.

## Context

Plugin Release transitions retain canonical Active Sets and Generation Specs,
and every `turn_started` event records a Generation Spec digest. Operators could
previously use those records only by reading private JSON files and preserving
the one-time upgrade output. That made manual rollback discovery and Session
audit unnecessarily fragile.

## Decision

The Harness exposes a read-only provenance surface:

- `plugins history` validates and lists the current and retained Active Sets;
- `plugins inspect --active-set <digest>` validates one exact Active Set and
  prints its Plugin Set, Releases, Manifests, Receipts, and Instances;
- `generations inspect --digest <digest>` validates one exact Generation Spec
  and prints its closed Host, policy, Plan, Plugin Set, artifact, and grant
  digests; and
- `sessions provenance --session <id>` lists each Turn's Generation digest and
  classifies its Spec as `available`, `missing`, or `invalid`.

The File Session Module owns validation of its private store and projects only
`turn_started` records. The Agent Loop owns interpretation of its exact event
payload. The CLI joins those two projections without printing user input or
other Session payloads.

Every content-addressed record is parsed canonically, matched to its requested
digest, checked for regular-file and symlink constraints, and—where
applicable—closed against immutable Store receipts and the product Plugin
Profile Catalog. Directory names are not trusted as proof of authority.

## Consequences

- A lost `previous-active-set` output can be recovered from validated history.
- Session audit can distinguish an available Spec from a missing or corrupted
  record without starting the App.
- Inspection never writes, repairs, deletes, activates, rolls back, or acquires
  a Generation lease.
- ADR 0014 closes local cross-process authority fencing. Retention policy,
  garbage collection, automatic repair, distributed coordination, overlap, and
  automatic rollback remain separate slices.

## Rejected alternatives

Parsing the private Session store in the CLI would duplicate Module ownership.
Interpreting Agent payloads in the Session Module would violate its opaque-event
contract. Treating filenames as valid digests would bypass canonical authority
validation.
