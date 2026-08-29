# Worktree Provider Plugin card

- **Plugin:** `lenso.agent.worktree-provider`
- **Capability:** provides `lenso.agent.worktree@1` and
  `lenso.agent.tool-provider@2`
- **Job:** allocate a retained isolated Git checkout before a configured
  mutation child starts, then let the parent review and integrate an exact
  revision explicitly.
- **Durable facts:** Git branches, commits, and worktree files. The allocation
  registry and retained review digest are Generation-local facts.
- **Authority:** one configured source repository, one delegated worktree root,
  and a readiness-pinned Git executable. It has no arbitrary Process or shell
  authority.
- **Model-visible Tools:** `list_worktrees`, `review_worktree`, and
  `integrate_worktree`. Review content includes the exact commit and diff digest
  required by integration, so the model does not depend on transport metadata.
- **Failure boundary:** invalid source identity, duplicate task, capacity,
  unavailable Git, dirty checkout, changed review, dirty parent, conflict,
  timeout, cancellation, and output overflow all fail closed.
- **Removal:** removing the Plugin removes mutation-child allocation and the
  three parent Tools from the next immutable Generation. It does not delete
  retained branches or worktree files implicitly.
