# Tool Hook and approval Module cards

Status: implementation baseline for unified Tool interception and the opt-in
one-shot approval workflow.

## `lenso.agent.tools` and `lenso.agent.workspace-read-tools`

- **Deletion boundary:** removing Hook requirements restores the previous Tool
  execution path; Tool catalogs, Providers, the Agent Loop, and Kernel remain.
- **Owned facts:** normalized Tool arguments, ordered Hook invocation,
  monotonic `deny > ask > allow` aggregation, execution correlation, and one
  terminal post observation.
- **Provides:** `lenso.agent.tools@2`.
- **Requires:** `many` `lenso.agent.tool-hook@1`, plus their existing Tool
  Provider or workspace-read requirements.
- **Final authorization:** remains with the selected Tool Provider. Hooks may
  only tighten admission and do not replace resource canonicalization or an OS
  security boundary.

## `lenso.agent.approval-hook`

- **Deletion boundary:** removes pending-action creation and approval checks;
  both Tool Runtimes and every Tool remain independently composed.
- **Owned facts:** exact-name allow/ask/deny policy, pending approval identity,
  Generation and action digests, one-shot consumption, rejection, and terminal
  status.
- **Provides:** `lenso.agent.tool-hook@1` (`before_execute`, `after_execute`).
- **Requires:** none.
- **Configuration:** durable directory, default decision, disjoint exact Tool
  lists, and record bound.
- **Lifecycle/resources:** activation verifies the durable directory. Each
  update holds a cross-process file lock and atomically replaces the bounded
  state document. Missing, corrupt, or unavailable state fails closed.
- **First behavior:** `ask` returns an approval ID without invoking the Tool.
  `approvals approve <id>` changes only that exact action to approved; its next
  exact retry consumes the grant once. Rejection denies that exact action for
  its App Generation.

## Selection

`plugins enable approval --evidence <review>` adds one Hook provider and binds
it to both `tools` and `restricted-read-tools` in the candidate immutable App
Generation. The bundled policy allows `read_text` and asks for every other Tool
name. This means direct Tools, outer `run_code` and `delegate`, Code Mode nested
reads, and child-Agent reads all enter the same Hook mechanism; it does not
mean every call must receive the same decision.

`plugins disable approval` switches back to a Generation with no Hook binding.
The first slice intentionally uses approve-then-retry instead of suspending a
live Turn. Tool arguments are durable and operator-visible; do not put secrets
in them.
