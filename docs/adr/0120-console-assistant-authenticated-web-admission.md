# ADR 0120: Console Assistant authenticated Web admission

Status: Accepted

A Console assistant member is a signed Auth user, never an unsigned HTTP subject.
The ordinary Console Agent proxy forwards the assertion extracted from its
successful Auth invocation as `x-lenso-actor-assertion`: URL-safe base64 without
padding of the existing `ActorAssertion.to_wire()` JSON. It overwrites any
client header. Agent requires both its existing proxy bearer and an immutable
issuer/public-key verifier. It verifies user kind, signature, validity and Agent
Run Turn audience before admitting member routes. Every identity-aware target
provider verifies its own operation audience independently. This extends ADR
0112's ownership transport to ordinary Web ingress; it does not add a new identity
system or transfer signing secrets to Agent.

Kernel preserves a sealed assertion only for an exact Capability/Operation
audience. The `ask_user` chain therefore also requires
`lenso.agent.tools@2:execute_stream` and
`lenso.agent.tool-provider@2:execute`, in addition to
`lenso.agent.user-interaction@2:ask`, `pending` and `answer`. Hook invocations
need their own `lenso.agent.tool-hook@1:before_execute` and `after_execute` audiences to retain
the assertion for audit. Missing intermediate audiences fail closed; forwarding
an unsigned subject or bypassing Kernel filtering is not a substitute.

The Web command captures one immutable assertion and owner pair `(issuer,
subject)`. Request cancellation and pending interactions are keyed by that owner
pair plus the request identifier. Session history, renaming, compaction, forks,
attachments and Turn execution use actor-bearing Host leases. Foreign Session
identifiers remain indistinguishable from missing identifiers at the Session
provider. Local operator sessions remain in the original ownerless scope.

Turn scheduling is above Kernel and Generation routing. Separate Session Turns
may hold separate immutable Generation leases concurrently; a Session identifier
has one running Turn at a time. Capacity, per-user running and waiting limits,
FIFO or owner round-robin selection, and bounded queue versus immediate rejection
are explicit Host configuration. Cancellation and interaction commands do not
wait behind an upstream model response. A disconnected SSE receiver cancels its
Turn lease. This is scheduling and durable data ownership, not an OS sandbox;
native Rust, Bun and Process Plugins remain trusted code.

Member ingress fails closed for shared operator capabilities without a member
ownership contract: Terminal commands, Plugin authoring/control, connection
consent, direct Tool execution, shared MCP context and aggregate task or activity inspection.
These capabilities must acquire their own owner-aware authorization paths before
being enabled for members. Hiding a Console navigation entry is not that path.
Shared cross-session Memory recall and storage are also disabled for signed
members until Memory has its own verified owner contract. Member Tool policy is
configurable and defaults to empty; this slice admits only the audited `ask_user`
Tool backed by the authenticated User Interaction provider. A global Tool grant
alone does not admit Process, subagent, task or workspace operator state.

The Host keeps a named immutable Web Agent/Model routing ceiling of 1024. The
configurable Web scheduler strictly narrows that ceiling and owns fairness and
rejection. Session calls retain one writer permit with a bounded mailbox.
Generated Capability default admission contracts and other surfaces remain
unchanged. For Sessions generated at Turn start or editing, the worker waits for
an actor Session-lock acknowledgement before opening a stream or exposing that
Session identifier in an event.

The original Local, Bearer and HostAuthorized entry modes preserve existing
single-operator compatibility, and default scheduling preserves one running Turn.
