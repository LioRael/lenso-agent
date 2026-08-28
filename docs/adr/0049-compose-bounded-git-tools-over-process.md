# ADR 0049: Compose bounded Git tools over Process

- Status: accepted
- Date: 2026-08-29

## Context

A coding Agent needs repository status, diffs, history, staging, and commits.
Exposing the entire `git` argv surface as one Tool would also expose destructive
history and workspace operations such as reset, clean, checkout, and push. A
second subprocess implementation inside a Git Plugin would duplicate executable
resolution, workspace containment, environment filtering, output bounds,
timeouts, cancellation, and child cleanup already owned by the Process Plugin.

Git is also not an Agent Loop concern. Different Profiles should be able to add
or remove repository authority without changing the Loop or Kernel.

## Decision

The bundled `lenso.agent.git-tools` Plugin consumes exactly one
`lenso.agent.process@1` provider and contributes five semantic Tools:

- parallel-safe `git_status`, `git_diff`, and `git_log`;
- exclusive `git_stage` for explicit literal repository-relative paths; and
- exclusive `git_commit` for already staged changes and one bounded message.

The Plugin always invokes the provider-authorized `git` executable with a
structured argv and `cwd = "."`. It uses literal pathspecs, rejects parent and
absolute paths, prevents implicit whole-repository staging, disables external
diff commands, and caps history and commit-message inputs. Commit disables Git
hooks and signing so a semantic Tool call cannot unexpectedly execute repository
code or an external signer.

The Plugin does not expose reset, clean, restore, checkout, branch mutation,
merge, rebase, tag mutation, fetch, pull, or push. A future operation must earn
its own semantic Tool and authorization review. `git_stage` and `git_commit`
still traverse the ordinary Tool Hook seam, and the provider remains the final
authority for Git arguments. An approval Hook cannot widen either provider.

The Host links and configures the Plugin but does not activate it by default. A
coding Profile selects configured `lenso.agent.process.native` and
`lenso.agent.git-tools` Instances. Removing the Git Instance removes all five
Tools; removing Process makes the candidate App fail before readiness rather
than silently falling back to direct child execution.

## Consequences

- Git gains a smaller, model-legible Interface without becoming a special case
  in Agent Loop, Tools Runtime, or Kernel.
- Process safety and lifecycle behavior remain local to one provider.
- Repository hooks and signing must be run through a separately authorized
  Process Tool or future dedicated workflow; `git_commit` intentionally does
  not run them.
- Git reads may still reveal repository content and Git itself is trusted native
  code. This Plugin is not a hostile-workspace sandbox.
- Profiles can configure a distinct Process root and Git limits through their
  selected Plugin Instances while keeping configuration out of Profile files.

## Rejected alternatives

Adding Git methods to Agent Loop couples one product workflow to every Agent.
Wrapping arbitrary `git` arguments gives the model the same authority as the
generic Process Tool with a misleadingly safer name. Implementing Git directly
with `std::process` duplicates the Process seam. Making Git a new Capability is
premature because there is currently one consumer shape—the model-facing Tool
provider—and no second Adapter requiring a stable cross-Plugin contract.
