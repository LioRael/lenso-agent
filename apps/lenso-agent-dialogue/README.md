# Lenso Agent Dialogue starter

`lenso-agent-dialogue` is an optional, explicit non-Coding composition. It is
not part of `lenso-agent-foundation` and is not selected by the legacy Coding
Host.

It selects exactly one deterministic fixture Model, one Agent Loop, durable
SQLite Session and Memory Providers, bounded local Artifact storage, the
Prompt runtime, an empty Tool runtime, and the headless CLI surface. It does
not select a Tool Provider, workspace Plugin, network Plugin, coding prompt,
or messaging channel.

```sh
cargo run -p lenso-agent-dialogue -- run "Answer directly: selected dialogue starter"
cargo run -p lenso-agent-dialogue -- plan
```

The selected Plan is written under the explicit home as runtime evidence. The
runner starts that exact Plan, so it does not publish the legacy Host Catalog
or inherit its defaults. The fixture Model makes the starter a local behavior
proof; select a different Model and execution Host when building a real Agent.
