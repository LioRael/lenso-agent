# Lenso Agent Foundation

`lenso-agent-foundation` starts a new Agent project with no product behavior.
It publishes a visible, product-neutral Host Catalog, then resolves the
selected Plugin Root without linking, enabling, installing, or running a
model, Loop, Tool, memory store, system prompt, or Surface.

```sh
mkdir support-agent
lenso-agent-foundation init --root support-agent
lenso-agent-foundation check --root support-agent --json
```

The initial check deliberately reports `blank`. That is a successful empty
Foundation, not a degraded Coding Agent: it has no hidden fallback model or
default Chat behavior.

The Foundation Catalog only supplies optional attachment points. It does not
choose a Provider, bind capabilities, grant permissions, or start execution.
Use an explicit execution Host or App authoring flow to select concrete
Plugins. That Host owns its stronger slot cardinality, bindings, adapters,
authorization, and lifecycle policy.

`check --json` is intended for authors and CI diagnostics. It reports the
resolved Plugin source, runtime package revision, entrypoint, runtime profile,
execution class, and exact Capability binding selection without printing
Plugin configuration or credentials. It also emits an explicit authorization
boundary: a blank Foundation grants no runtime, provider, credential,
dynamic-resource, or external-side-effect authority.

`init` refuses to overwrite an existing different Host Catalog, so an existing
Agent Home or App remains owned by its current Host. `check` is read-only and
uses the public Plugin Root resolver; a resolved result proves structural
composition only, not runtime readiness.
