# ADR-0073: Compose bounded GitHub workflows over Process

Status: Accepted

## Context

Local Git tools do not cover GitHub Issues, pull requests, or Actions runs.
Giving the model a shell or an unrestricted `gh` command would make repository
scope, mutation availability, argument validation, Tool policy, and provenance
implicit. Reimplementing GitHub authentication and HTTP semantics in the Agent
Loop would duplicate the established GitHub CLI credential owner and put a
vendor workflow into the runtime core.

## Decision

- `lenso.agent.github-workflows` is an optional Tool Provider Plugin. It binds
  to exactly one selected `lenso.agent.process@1` Provider and requires that
  Provider to authorize the `gh` executable.
- Configuration names a sorted, unique, bounded set of `owner/repository`
  identities. Every Tool call repeats and validates one of those exact
  identities; current-directory inference is not authority.
- The read-only catalog exposes Issue read, pull-request read with check rollup,
  and bounded Actions-run status. Read Tools are parallel-safe.
- Explicit `enable_mutations` adds Issue create/comment/close, pull-request
  create/merge with an explicit method, and Actions rerun. Mutation Tools are
  exclusive and remain subject to the composed Tool Hook and approval policy.
- The Plugin constructs fixed `gh api`, `gh pr`, and `gh run` argument vectors.
  It exposes no shell, arbitrary endpoint, arbitrary CLI flag, force merge, or
  implicit repository selection.
- Authentication remains owned by the installed `gh` process. The Plugin does
  not read, persist, serialize, or return tokens.

## Consequences

- GitHub collaboration is available as semantic, auditable Tools while the
  existing Process Provider retains executable, environment, timeout, output,
  and Workspace policy.
- Repository expansion or mutation enablement is a visible Plugin Root and
  Generation change.
- GitHub Enterprise endpoints, projects, releases, deployments, labels,
  reviewers, and administrative operations are not silently admitted by this
  slice.

## Proof

Plugin tests verify its generated Descriptor, read-only versus mutation Tool
catalog, exact command construction, and fail-closed repository allowlist. Host
tests resolve the optional Plugin through the ordinary Tool Provider slot and
prove its exact Process binding; the Plugin remains absent from the default
App.
