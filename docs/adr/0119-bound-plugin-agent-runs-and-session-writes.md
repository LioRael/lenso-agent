# ADR 0119: Bound plugin Agent runs and explicit Session writes

Status: Proposed — locally implemented, pending coordinated review

## Decision

Reuse the existing Agent Loop and its exact retained Generation dependencies.
A task Kernel selects Loop, `lenso.agent.plugin-run` and an endpoint-free surface.
The source-generated boundary Plugin forwards only Host-captured Model, Tools,
Prompt and an explicitly admitted Session client. It introduces no second loop,
provider discovery, model fallback, signing key or Tool execution authority.

Console's existing Workspace Service AI adapter exposes separate `complete` and
`run` operations. Before dispatch, its durable usage ledger reserves every
possible Model call at the exact quoted provider/model/Generation/catalog input
ceiling and known Host price revision. The boundary counts every real Model open,
including Loop retries, buffers bounded output to terminal, requires final usage
and fails if the reserved call or token bound is exceeded. Successful tasks settle
aggregate measured usage. Cancellation or unknown evidence retains the entire
reservation; successful terminal receipts permit explicit concurrency recovery
without refund. A changed Generation/provider invalidates admission.

Background history is an in-memory Session facade for this one task. It is
bounded, discarded after Kernel shutdown and never appears in assistant history.
Memory observes/recalls are empty; writes, Artifacts and compaction are denied.
No attachments are accepted. These limits fail visibly. Tools remain the original
aggregate with its Hook/provider authorization, narrowed to an administrator's
allowlist. Request-mode approval overrides inherited Full/Assisted mode; absence
of an approval surface fails closed. Nested Agent and code-mode Tools are denied
because their secondary calls cannot be covered by this task's Model meter.
Trusted custom Tools must not introduce unmetered Model calls.

## Session authority

The Session native SDK adds `SessionNamespace` baggage (consumer, verified user,
Host project), without changing the public Session wire descriptor. Authenticated
SQLite includes this namespace in its owner identity after verifying the original
operators Actor. A missing namespace preserves the existing issuer/subject owner
encoding. Root assistant history remains separate from plugin-scoped history.
File Session is not qualified for authenticated scoped writes.

Console stores only Session references and grants in a separate authority SQLite
file, preserving the completion ledger's existing version-1 tables. Both use
WAL/FULL; configuration changes and foreign/incomplete files fail closed.
`open_session` explicitly creates isolated plugin history. `register_session`
verifies ownership of an existing assistant Session before binding it; only an
administrator-designated owner consumer may register it or issue a grant.
`grant_session` is an explicit privileged owner operation, limited to one exact
recipient consumer/user/project/Session and at most one hour. No run can create
or grant a persistent Session implicitly. Grant IDs confer no authority outside
the original signed Actor and Kernel caller binding. Owners may revoke grants;
revocation/expiry/live Credential State are checked before dispatch, every 250 ms
while pending and again before settlement. Accepted side effects cannot be undone
by later revocation. Lost authority fails closed and cancels native work.

The Host keeps its existing control token and provider credentials. Installation
of the assistant grants no AI or Tool permissions. The management MCP and the
assistant remain independently installable Console Plugins; Agent retains its
standalone workspace. Framework and Relay contracts, existing realm/audience
semantics, interactive Loop and history handlers are unchanged.

## Evidence

Focused native Host tests exercise two real Loop Model calls plus an uppercase
Tool, zero persistent background history, call/tool limits, scoped resume and
consumer/user/project denial. Coordinated separate-process Console/Agent fixtures
exercise the actual Workspace Service binding, explicit existing-Session grants,
revocation during a pending run, known-price reservation and existing completion
recovery. All Models and credentials are synthetic. No publication or paid call.
