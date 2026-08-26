# ADR 0029: Compose constrained Code Mode as a Tool

## Status

Accepted.

## Context

The Agent can already submit direct Tool calls and bounded parallel Tool waves. Some tasks are more
compact and deterministic when one small program transforms results and decides which reviewed
Tools to call. Giving model-authored code ambient filesystem, process, network, or root Tool access
would bypass App Composition and turn one strong entrypoint into an authority escalation. Making
Code Mode part of the Kernel would also couple product-specific orchestration to the portable
runtime.

## Decision

The Harness admits Code Mode as an optional native Tool Provider Module.

- The reviewed `code-mode` Plugin contributes one exclusive `run_code` Tool. Removing the Plugin
  removes the model-visible surface in the next immutable App Generation.
- The provider evaluates bounded Lua 5.4 source with no `io`, `os`, `package`, `debug`, filesystem,
  process, or network library. App-owned configuration bounds source bytes, VM memory, executed
  instructions, output bytes, nested calls, and parallel nested calls.
- The program receives only `tool(name, arguments)` and `parallel(calls)`. Both invoke one
  explicitly bound `lenso.agent.tools@2` provider; there is no registry lookup or ambient Host
  access.
- The first Composition binds Code Mode to the shared narrow `restricted-read-tools` Runtime, so code
  can call only `read_text`. Root workspace-edit, process, Skill, subagent, and future Tool Plugins
  are not inherited.
- Nested calls use the bound Tool catalog for name admission and execution classification. Ordered
  parallel-safe runs use a bounded pool; exclusive calls are barriers. The returned Tool metadata
  contains an ordered nested-call transcript, which the parent Session records in its ordinary
  `tool_result` fact.
- This in-process interpreter is trusted product code with capability and resource confinement. It
  is not a hostile-code security sandbox. Independently authored or adversarial code requires a
  reviewed Wasm or isolated-process Execution Adapter.

## Consequences

Code Mode is runtime-toggleable through the existing Desired State and App Generation switch, but
its authority remains narrower than the root Agent. Adding mutation, process, or network access is
a later App-owner decision and must compose approval and Hook policy without changing this Module's
ambient access. A VM failure fails the `run_code` call and does not retry nested effects.

## Rejected alternatives

Binding Code Mode back to the root Tool Runtime creates a Capability cycle and silently inherits
future authority. Exposing a shell or native process is not a code sandbox. Implementing only a
JSON batch omits result-driven program flow, while putting evaluation or Tool lookup in the Kernel
breaks the ordinary Module boundary.
