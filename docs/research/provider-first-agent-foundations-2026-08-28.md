# Provider-first Agent Foundations (2026-08-28)

## Decision direction

Lenso Agent Harness should follow a provider-first shape similar to DeepSeek
Harness (DSH): the Harness owns small capability seams and lifecycle
invariants, third-party Plugins are first-class Adapters at those seams, and
the default Profile selects official Adapters that provide a useful local
experience. An official Adapter must not gain a private path into the Agent
Loop that a third-party Adapter cannot use.

This direction does **not** mean copying DSH's package count or exposing its
internal vocabulary to Lenso users. The public experience remains Profile +
Plugin configuration. Persistence coordinators, compaction checkpoints,
memory consolidation jobs, and transport clients remain implementation
details behind Plugin interfaces.

## System instruction is a Session instrument

The System Prompt is not an optional Prompt contribution. Every newly created
Session must install one non-empty System Instruction before its first user
input. A Profile may replace or edit the persona layer, and Plugins may
contribute bounded instruction sections, but removing every contribution must
still leave the Harness's safe built-in instruction.

The required initialization order is:

1. resolve the Profile and immutable App Generation;
2. obtain the built-in Harness instruction and bounded Plugin contributions;
3. render and validate one `SystemInstructionSnapshot`;
4. durably create the Session and record the instruction manifest and digest;
5. emit the typed `session_start` lifecycle fact;
6. admit the first user input.

A resumed Session reuses its installed snapshot. Starting a new Session under
an edited Profile installs a new snapshot. Dynamic workspace state, recalled
Memory, tool catalogs, and compaction summaries are request context; they do
not silently rewrite the Session's base System Instruction.

The current ADR-0003 decision allows an empty aggregate and the current Agent
Loop assembles Prompt content while executing each Turn. Both are incompatible
with this direction and require a superseding implementation ADR before the
runtime changes.

## Real seams and default Adapters

A seam is justified only where at least two real Adapters exist or are
delivered together. Each official default below therefore ships with a
conformance interface intended for third parties.

| Harness seam | Official default | First-class alternatives |
| --- | --- | --- |
| Session persistence | local SQLite event log with JSONL import/export | JSONL, remote database, hosted Session store |
| System instruction | editable Profile-owned Markdown plus built-in fallback | organization policy, game persona, remote prompt registry |
| User interaction | TUI/CLI input | Agent Channel, web UI, remote approval system |
| Context compaction | bounded summary plus retained recent tail | domain compactor, remote summarizer, model-free pruning policy |
| Memory | local SQLite + FTS, provenance, explicit forget | MCP memory, hosted vector/search system, organization knowledge store |
| Lifecycle hooks | in-process typed observers and a bounded command Adapter | telemetry, policy, workflow, hosted event sink |
| Secrets | development environment Adapter, then macOS Keychain | encrypted file, 1Password, Vault, cloud secret managers |

DSH uses this shape for Session persistence: one persistence contract has JSONL
and SQLite backends while keeping Session events as the durable fact model.
Its compaction engine is an optional capability with automatic and manual
entry points, durable transaction events, and tool-call/result pairing
invariants. Its third-party Memory examples connect providers through the MCP
client without making any one vendor part of the Agent Loop.

Primary sources:

- [DSH Session persistence](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/session/session-persistence/README.md)
- [DSH SQLite persistence](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/session/session-persistence-sqlite/README.md)
- [DSH compaction subsystem](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/subsystems/compaction.md)
- [DSH third-party MCP Memory examples](https://github.com/deepseek-ai/deepseek-harness/blob/master/examples/mcp-memory/README.md)
- [DSH System Prompt composition](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/core/system-prompt/README.md)

## Memory and compaction remain different

Session persistence stores canonical events. Compaction creates a replaceable
model-context projection. Memory curates cross-Session knowledge. A default
implementation may store all three in one SQLite database, but sharing a
database does not merge their interfaces or authority.

The default compactor must preserve the complete Session log, tool-call/result
pairing, retained-tail bounds, cancellation, and durable start/commit/failure
facts. A third-party compactor replaces only the compaction Adapter.

The default Memory Adapter starts with SQLite + FTS rather than requiring an
embedding service. It records scope, source Session/event provenance,
confidence, and deletion state. Extraction and consolidation are staged jobs;
model-facing `remember`, `recall`, and `forget` tools are consumers of Memory,
not the Memory seam itself. An MCP server may provide those tools directly or
an Adapter may translate a remote memory product into the native Memory seam.

## MCP mapping

An MCP server is not converted wholesale into one Tool Provider. The MCP
client Plugin owns transport, authentication, lifecycle, reconnect, progress,
and protocol namespaces, then maps each negotiated MCP capability into the
corresponding Harness seam:

```text
MCP tools       -> Tool Provider
MCP prompts     -> instruction/prompt contribution
MCP resources   -> resource/context provider
MCP elicitation -> User Interaction
MCP sampling    -> model delegation policy
```

Tool catalog changes produce a candidate App Generation and never mutate a
running Turn. This keeps third-party MCP integration first-class without
discarding protocol semantics that are not tools.

## Delivery order

1. supersede ADR-0003 and install a non-empty System Instruction at Session
   creation;
2. make Session inspection/provenance backend-neutral and ship SQLite beside
   the file Adapter;
3. add typed lifecycle and User Interaction seams with TUI and headless
   behavior;
4. ship the default compaction Adapter plus a conformance fake;
5. ship local Memory plus an MCP/remote Adapter proof;
6. add Git and richer Secrets Providers as Profile-selected Plugins.

Every seam must have contract tests that run unchanged against the official
default and at least one alternative Adapter. A feature is not third-party
ready merely because its Rust trait is public.
