# ADR-0059: Compose named child Agents by Profile

Status: Accepted

## Context

The subagent Tool Plugin already owns a bounded task registry, detached child
cancellation, durable child Sessions, additional Turn input, and terminal
results. Composition nevertheless binds every task to one hidden
`subagent-agent` Instance. A parent cannot choose between independently named
child execution lanes, and task facts do not identify which Agent ran them.

Putting a scheduler or mutable Agent graph in Kernel would violate the Host and
Plugin boundary. Encoding child identities in subagent Plugin configuration
would also duplicate the immutable Plan's binding authority.

## Decision

The official `code` and `code-sandbox` Profiles explicitly select two named,
read-only child Agent Instances: `lenso.agent.loop/researcher` and
`lenso.agent.loop/reviewer`. Each remains an ordinary Agent Loop Plugin
Instance whose Tool authority is composed by the Host Catalog.

`lenso.agent.subagent-tools` consumes `many` Agent and Turn Input bindings.
The generated typed ports retain each provider Instance key. At runtime the
Plugin requires both ordered provider sets to match exactly and fails closed if
they do not. `delegate` and `spawn_subagent` expose the resolved Instance keys
as a required enum and route only to the selected binding.

The selected Agent Instance is retained in running-task snapshots, Tool
content, and versioned task/result metadata. `send_subagent` uses that retained
identity to reach the matching Turn Input provider; callers cannot redirect an
existing task to a different child.

## Consequences

- Profile authors can add, remove, and name child Agents without changing the
  Tool Plugin or Kernel;
- the model sees only child identities present in the immutable resolved Plan;
- concurrent tasks may target different Agent Instances while retaining their
  own Sessions and cancellation; and
- the two read-only children retain restricted Tool authority, while ADR-0062
  composes separate `worker-a` and `worker-b` mutation lanes through the
  Worktree Provider.

## Proof

Host Plan tests prove both named Agents receive Agent and Turn Input bindings.
CLI installer tests prove both official coding Profiles select their child
Instances. Subagent unit and headless tests prove explicit selection, durable
identity in task/result metadata, correct additional-input routing, and the
existing bounded lifecycle behavior.
