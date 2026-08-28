# ADR 0035: Apply fenced Generation provenance garbage collection

## Status

Accepted.

## Context

ADR-0015 introduced a read-only Generation GC preview because Session writes,
Controller recovery, and Plugin authority could change reachability after the
scan. Long-lived source Hosts now retain exact Generation Specs and recovery
authority across restarts, so preview alone allows retired provenance to grow
without bound.

Deleting by age or by `Retired` status is unsafe. A retired Generation may
still be referenced by a durable Session, while Active, Draining, or Standby
records in any surface Controller remain recovery roots. A one-turn Host can
also persist new Session provenance while another process is collecting.

## Decision

The Harness exposes:

```text
generations gc --apply [--root <plugin-root>]
  [--session-database <sqlite-path> | --sessions <session-directory>]
```

The command inspects `.lenso/sessions.sqlite3` by default. `--sessions` selects
an explicit file Adapter directory.

Every Host that uses an existing Plugin authority root holds a shared
process-lifetime Generation GC lease. Apply takes the exclusive form of that
lease, waits for all such Hosts and Turns to exit, then takes the existing
exclusive Plugin authority fence. It rebuilds the mark set inside both fences;
the earlier `gc-preview` output is never deletion authority.

The mark set protects a Generation Spec when any of these roots references it:

- the current or retained Plugin Set lock;
- a non-retired record in any headless, TUI, Telegram, Discord, or combined
  channel Controller namespace; or
- a durable Session `turn_started` event.

Every referenced Spec must exist and validate before deletion begins. Apply
removes only unmarked canonical Generation Spec files. It then removes a
recovery Active Set record only when no remaining protected Spec or retained
Plugin Set uses that record's Plugin Set lock. Directory metadata is synced
after deletion. Reapplying the same plan is idempotent.

Controller records are not rewritten outside their compare-and-set owner.
Active Set history and Plugin Store manifests, Receipts, and Artifact objects
remain intact because they still support explicit rollback and require a
separate closure-aware collection contract.

## Consequences

- A running Host blocks apply instead of racing Session provenance or recovery.
- Active, Draining, Standby, Staged, and Ready records are protected across all
  surface namespaces; retired records alone are not roots.
- Corrupt directories, malformed authorities, unknown files, and missing
  referenced Specs fail closed before collection.
- Collection remains a local-filesystem Host operation, not a distributed
  lease or network-filesystem guarantee.
- Plugin Store Artifact GC and Controller record compaction remain deferred.

## Proof

Tests use a real child process to prove an exclusive collection waits for the
Host's shared lease. Headless integration creates two Generations, proves the
stopped Controller protects its active digest even after Desired State changes,
reconciles that Controller, applies collection against an empty Session root,
removes only the retired unreferenced Spec and recovery authority, preserves
the live Spec, and proves a second apply removes nothing.

## Rejected alternatives

Deleting directly from `gc-preview` preserves the original time-of-check race.
Using modification time ignores Session and Controller authority. Rewriting
Controller JSON from the Harness would bypass the control plane's CAS and
Supervisor epochs. Treating recovery authority or the entire Plugin Store as
disposable cache would break crash recovery and explicit rollback.
