# ADR-0043: Install the System Instruction at Session start

Status: Accepted

## Context

ADR-0003 treated the Prompt aggregate as optional, allowed an empty result,
assembled it during every Turn, and recorded only its contribution manifest.
That makes a Session's governing instruction depend on whichever App
Generation happens to resume it. It also prevents the durable Session from
proving the exact instruction that governed its requests.

A System Instruction is a Session instrument, not transient Turn context.
Profiles and third-party Prompt Providers may customize it, but every Session
must begin with a non-empty instruction and retain that instruction when later
resumed. Dynamic workspace state, Memory recall, tool catalogs, and compaction
summaries remain request context and do not rewrite this base instruction.

## Decision

The official Prompt aggregate always prepends a bounded `harness.base`
instruction. Optional Prompt Providers follow in explicit Composition order.
The Agent Loop also rejects an empty aggregate so replacement Prompt Plugins
must satisfy the same invariant without using an official private path.

When a Session is created, the Agent Loop assembles the Prompt once and writes
`session_created`, `system_instruction_installed`, and `turn_started` in one
optimistic append. The installed event precedes the first user input and owns:

- the complete rendered System Instruction;
- its `sha256:` content digest;
- the ordered contribution manifest;
- the App Generation Spec digest that installed it.

Every Model request includes the installed content as its system message. Its
`model_requested` event repeats only the instruction digest and contribution
manifest for convenient projection; the installed event remains authoritative.

Resuming a Session scans its complete event stream for exactly one valid
installed instruction and never calls the Prompt aggregate again. A pre-0043
Session with no installed event is migrated once on its first successful
resume. Missing content, invalid digests or provenance, malformed manifests,
and multiple installed instructions fail closed.

`lenso.agent.session@1` Descriptor `1.2.0` adds the
`system_instruction_installed` event kind. Session Adapters store the event as
an ordinary durable fact; they do not interpret Prompt semantics.

## Consequences

- A Session remains governed by one inspectable instruction across Host and
  Profile restarts.
- New Sessions see edited Profile instructions; existing Sessions do not.
- The durable log contains instruction content, which improves replay and
  auditability but means Session storage must be protected as sensitive data.
- Third-party Prompt and Session Plugins remain first-class because invariants
  are enforced at their public capability boundaries.
- Turn-specific context and future compaction or Memory Adapters stay separate
  from the immutable base instruction.
