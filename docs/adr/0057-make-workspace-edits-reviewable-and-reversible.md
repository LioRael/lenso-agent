# ADR-0057: Make Workspace edits explicitly reviewable and reversible

Status: Accepted

## Context

The coding Profile can edit files and show each Tool call in the TUI, but it
does not own a complete change set that a user or Agent can review, accept, or
restore. A Git reset or checkout would destroy unrelated dirty work. A generic
Tool Hook can observe edit arguments but would duplicate the Workspace Edit
Plugin's path authorization and infer another Plugin's mutation semantics.

## Decision

`lenso.agent.workspace-edit` release `0.3.0` owns explicit Workspace
checkpoints because it is already the final authority that resolves and writes
the affected files. Its Tool catalog adds:

- `checkpoint_create`, returning one opaque checkpoint ID;
- `checkpoint_review`, returning a bounded unified diff and conflict count;
- `checkpoint_accept`, preserving current files and deleting stored preimages;
  and
- `checkpoint_restore`, preflighting the complete change set and changing
  nothing when any target has an external content conflict.

`edit` and `create_file` accept an optional `checkpoint_id`. Configuration can
set `require_checkpoint = true`; the official coding Profile does so. The
Plugin stores each file's first UTF-8 preimage and the digests of every content
state produced under that checkpoint. A later edit may continue only from the
original or a recorded digest. Restore first validates every target and then
replaces each existing file or removes a checkpoint-created file.
It never invokes Git restore/reset and never removes a file whose current
digest was not produced under that checkpoint.

Checkpoint manifests live in the Host-configured Agent runtime directory,
outside the Workspace. A process lock serializes manifest transitions across
Host processes. Writes use temporary files, fsync, and rename. Checkpoints are
explicitly identified rather than inferred from Session or Generation context,
so concurrent Sessions cannot silently share a change set.

The TUI renders checkpoint reviews as a distinct semantic Tool card with
colored diff lines. The existing interactive Approval Hook allows create and
review, while accept and restore still require one exact inline approval.

## Consequences

- pre-existing dirty work is captured only when the Agent edits that exact
  file and is restored exactly rather than reset to `HEAD`;
- external content changes make restore fail before any file is mutated;
- deleting Workspace Edit removes mutation and checkpoint behavior together;
- accepting or restoring removes the durable preimages; and
- binary files, deletion tools, directory trees, Git index state, and process
  side effects remain outside this checkpoint contract.

## Proof

Plugin tests prove edit/create review and restore, required-checkpoint policy,
accept semantics, and all-or-nothing conflict rejection. Product tests resolve
the official coding Profile with checkpoint enforcement. TUI tests cover the
semantic checkpoint card. Removing the Workspace Edit Instance leaves the
read-only base App resolvable without a Kernel branch.
