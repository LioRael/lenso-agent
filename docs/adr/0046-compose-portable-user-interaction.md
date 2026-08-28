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

The Harness defines portable `lenso.agent.user-interaction@2` as a replaceable
Capability with three operations:

- `ask` registers one bounded interaction containing one or more questions and
  waits for their structured answers;
- `pending` lets a surface snapshot unanswered questions;
- `answer` completes an exact pending question.

The default `lenso.agent.user-interaction.local` Adapter uses an in-process,
bounded broker. It is independent of TUI rendering. The TUI Shell has a typed
Port to the same Adapter and temporarily replaces the composer with a bottom-
anchored question card. Other surfaces can replace or front the Adapter without
changing the Tool. Single-select options can expose an inline focused preview,
multi-select questions use explicit checked state, every question includes a
sticky Other path, and one request can contain several independently navigable
questions.

An accepted answer completes the blocked `ask_user` Tool invocation. It is not
a new conversational prompt and therefore must not create a User transcript
entry. The resulting Tool completion remains the durable conversation record.

`lenso.agent.ask-user-tools` projects the seam as one exclusive `ask_user`
Tool. It accepts one to eight identified questions with bounded option labels,
descriptions, optional single-select previews, and an explicit multi-select
flag. Answers contain stable question and option IDs plus optional Other text.
This structured contract replaces the v1 single prompt/string answer because
the answer cardinality and validation semantics are not backward compatible.

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
- One Tool invocation can collect several related decisions without inventing
  multiple Tool calls, while each pending interaction remains Generation-pinned.
