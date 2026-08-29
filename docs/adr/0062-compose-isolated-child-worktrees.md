# ADR-0062: Compose isolated child worktrees

Status: Accepted

## Context

Named mutation-capable child Agents need independent filesystems so concurrent
edits cannot race in the parent Workspace. Kernel must not acquire Git,
filesystem, scheduling, or merge authority, and a child Workspace path must not
become ambient authority for every Provider.

Allocating a checkout alone is insufficient. The Workspace read, edit, native
process, and sandbox process Providers must all observe the same scoped path,
while integration must remain a separate reviewed parent action.

## Decision

`lenso.agent.worktree-provider` owns Generation-local allocation facts and
provides `lenso.agent.worktree@1`. For Agent Instances listed as mutation lanes,
`allocate` creates one branch and Git worktree under the Agent Home runtime
directory. Other named children retain the current read-only Workspace.

The Provider resolves and pins the Git executable during readiness, then uses a
private bounded Git runner with null stdin, disabled hooks and color, bounded
stdout and stderr, timeout, and cancellation. This is constrained Provider
mechanics, not a generic shell Tool or security sandbox. A non-Git Workspace may
start; mutation allocation fails closed when Git cannot establish its base
commit.

The subagent Tool Plugin requests an allocation before starting a child. It
builds task provenance from the parent owner and immutable Generation, replaces
only the child Workspace scope, and removes the parent's Tool-call owner before
the child Agent runs. Detached tasks own their cancellation token; completion of
the spawning Tool call cannot cancel background work.

Workspace read, Workspace edit, native Process, and sandbox Process Providers
accept a different Workspace scope only when it is a canonical Git worktree
below the Host-configured delegated root. Mutation Agents use a distinct Tools
runtime so the root Tools runtime does not form an activation cycle. Its bounded
Tool and Process admissions allow two child lanes to make progress without
turning either surface or Kernel into a scheduler.

The Provider also exposes three parent Tools:

- `list_worktrees` projects retained allocations;
- `review_worktree` requires a clean child checkout and locks its exact HEAD and
  diff SHA-256, returning both values in model-visible content alongside the
  bounded diff; and
- `integrate_worktree` requires that retained review, an unchanged clean child,
  and a clean parent Workspace before a no-fast-forward merge. Conflicts abort
  the merge. Successful integration removes the checkout and integrated branch
  without force.

The allocation registry is Generation-local. Branches and worktree files remain
inspectable if a surface reconnects, but restart recovery or automatic orphan
cleanup is not claimed by this decision.

## Consequences

- mutation children can edit and commit concurrently without changing the
  parent Workspace;
- every filesystem and process action derives authority from the same child
  Workspace scope;
- review and integration are explicit parent actions locked to one immutable
  child revision;
- dirty, changed-after-review, conflicting, over-capacity, and unavailable-Git
  states fail closed; and
- later durable task recovery may reconcile retained Git worktrees, but cannot
  move their ownership into Kernel.

## Proof

The source-first Worktree Capability has generated Descriptor, Schemas, and Rust
bindings with freshness checks. Provider tests allocate two isolated checkouts,
reject a mismatched review digest, and integrate only the exact reviewed commit.
Headless official-profile coverage runs two named mutation children through the
real Agent, Tool, Workspace, Process, and Git paths, proves both commits remain
in distinct retained worktrees, and proves the parent Workspace is unchanged.
