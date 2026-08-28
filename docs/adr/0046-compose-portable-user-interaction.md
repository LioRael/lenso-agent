# ADR-0046: Compose portable user interaction

Status: Accepted

## Context

An Agent sometimes cannot continue without a choice or missing fact from its
user. Making `ask_user` a TUI callback would couple Tool execution to terminal
state and prevent web, chat, remote approval, or test Adapters from providing
the same behavior. Sending a question only on the Agent response stream also
cannot carry the answer back into the blocked Tool invocation.

Non-interactive runs need a defined result. Waiting for a timeout when no
surface can answer is misleading and makes headless automation hang.

## Decision

The Harness defines portable `lenso.agent.user-interaction@1` as a replaceable
Capability with three operations:

- `ask` registers one bounded question and waits for its answer;
- `pending` lets a surface snapshot unanswered questions;
- `answer` completes an exact pending question.

The default `lenso.agent.user-interaction.local` Adapter uses an in-process,
bounded broker. It is independent of TUI rendering. The TUI Shell has a typed
Port to the same Adapter, displays a question in the transcript, and submits
the ordinary composer input as its answer. Other surfaces can replace or front
the Adapter without changing the Tool.

`lenso.agent.ask-user-tools` projects the seam as one exclusive `ask_user`
Tool. It accepts a question, optional unique choices, and an explicit
free-form policy.

Only a Host-created Invocation Context for an interactive surface carries
`lenso.agent.interactive-surface@1`. The local Adapter rejects `ask` with
`unavailable` before allocating pending state when the marker is absent. The
Tool maps this to stable `interaction_unavailable`; headless and channel runs
therefore fail immediately unless their composition supplies interaction.

Pending questions are scoped to the immutable Generation that admitted the
Turn. The TUI answers through the same Generation lease, so an online Plugin
change cannot route an answer into a newer Adapter instance.

## Consequences

- Tool and Agent Loop code contain no terminal widgets, input queues, or chat
  transport state.
- TUI interaction is usable now, while web, mobile, channel, and remote
  Adapters have a stable protocol to implement later.
- A non-interactive surface has explicit behavior instead of a hidden timeout.
- The first version intentionally supports one-at-a-time TUI presentation;
  the Capability remains bounded and can queue multiple independent requests.
