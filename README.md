# Lenso Agent

An embedded business Agent for Bun applications. One `@lenso/agent` package,
ordinary async APIs, and the real Pi Agent loop. It does not start a server,
depend on Console, install a User table, or own the host's database.

## Run locally

Requires Bun 1.4.2 or newer. No Rust, paid provider, or credentials are needed:

```sh
bun install --frozen-lockfile
bun run build
bun run typecheck
bun test
bun run example
bun run check:consumer
bun run pack
```

The offline example uses a scripted Pi provider, real Lenso Manage dispatch,
real Auth actors, and SQLite. It reads a note, proposes a version-bound write,
confirms it, demonstrates cancellation and revoked permission, reopens history
and pending actions, and classifies a missing effect receipt as unknown without
replaying the write. The fixture is not a replacement Agent implementation.

The example is a one-shot acceptance scenario, not an interactive assistant.
Each invocation creates and removes its own temporary database. Its restart
checks close and reopen the host and database in the same Bun process; they do
not kill the process or continue a session across separate example invocations.

Source lives in the public [LioRael/lenso-agent](https://github.com/LioRael/lenso-agent)
repository, separate from the read-only Rust reference repository
`LioRael/lenso-agent-rust`. The package remains `private` to prevent accidental
npm publication. Package publication and deployment need separate authorization.

## Public entries

| Entry | Responsibility |
| --- | --- |
| `@lenso/agent` | `createAgentService`, `createMemoryStore`, domain types, safe errors |
| `@lenso/agent/pi` | Explicit Pi configuration, without replacing tool execution |
| `@lenso/agent/manage` | Exact-instance, explicitly selected Manage tools |
| `@lenso/agent/sqlite` | Borrowed/owned Bun SQLite store and explicit migration |
| `@lenso/agent/plugin` | Thin Lenso dependency/lifecycle wiring |
| `@lenso/agent/fetch` | A handler for the host's existing Fetch listener |

The service exposes `createSession`, `readSession`, `startRun`, `getRun`, `events`,
`cancelRun`, `confirmAction`, `cancelAction`, `recoverInterrupted`, and `close`.
`startRun` returns `{run, events, done}`; it does not wait for the model to finish.
`readSession` returns the current history, runs, tool calls, and actions.
`pack` leaves a reproducible local candidate in `artifacts/`.

The host supplies a model, storage, profiles, a trusted identity resolver, and a
tool source. Identity includes subject, scope, application, and target.
`target` is the exact running Lenso `instanceId`, not a browser-selected workspace
or a model argument. Resolve and revalidate browser target hints server-side.

Profiles contain application instructions, explicit stable tool names, and an
optional context supplier. Context carries source labels and is external data,
not authority. Profiles do not grant permissions. Select profiles explicitly;
installed plugins are not scanned or automatically exposed.

## Tools and confirmation

`createManageToolSource` uses the existing `createAgentTools`, real shared
schemas, and `createManageAdapter` invocation. Explicit aliases name selected
Operation declarations, not unstable `operation_N` catalog indexes. Binding
preserves the exact Plugin object and service `this`.

Catalog authorization is not execution authorization. The host must provide
current policy checks and trusted operation bindings; real services must still
check their Auth audience, owner/resource policy, and current evidence. Actors,
`approved: true`, plugin IDs, and target claims from model JSON are not evidence.
Approval metadata remains a separate host-owned policy and fails closed if its
trusted callback is absent.

Writes and unknown effects require confirmation. A durable action binds the
identity, precise application/instance/plugin/operation, raw schema-valid
arguments, current precondition, expiration, run, and tool call. Confirm only
through an authenticated host entry using the action ID and its displayed
digest; the request cannot replace its arguments. A fresh authorization and
precondition check precede an atomic pending-to-executing transition.
Double-clicks cannot admit a second execution.

The precondition supplier detects changes between proposal and confirmation.
It is not an atomic business lock: **the actual service must enforce the
expected version at the effect**, for example with `UPDATE ... WHERE version = ?`.
The Notes example does so. Different service/resource instances must use
distinct target identities. Host code and plugins are trusted in-process code,
not a sandbox.

Expired, cancelled, consumed, rejected, and unknown actions have explicit states.
An action proposed during a run can be confirmed after that run settles; another
active operation in its session returns `session-busy`. Confirmation never
suspends a long-lived model Promise waiting for a UI button.

## Storage, events, and recovery

```ts
import { Database } from "bun:sqlite";
import { createSqliteStore, migrateAgentDatabase } from "@lenso/agent/sqlite";

const db = new Database("agent.sqlite");
migrateAgentDatabase(db); // explicit host-controlled installation step
const store = createSqliteStore(db); // borrowed; close does not close db
```

Migration 001 uses `lenso_agent_*` tables and its own schema version. It does not
change host `user_version`, busy timeout, or other pragmas. Transaction callbacks
are synchronous and atomic. Separate SQLite handles use `BEGIN IMMEDIATE`;
`SQLITE_BUSY` propagates without hidden retry. The host owns database connection
and busy-timeout policy. Default construction does not create tables.

| Capability | Guarantee and limit |
| --- | --- |
| History | Accepted user messages persist atomically with run admission, including context-loading failures. Safe finalized outputs, run/call states, actions, and results persist. |
| Live events | Ordered IDs, sequence, timestamps, run/session/call/action correlation. Bounded subscriber buffers. No persisted delta log or historical event replay. |
| Reconnect | Fetch a state/history snapshot, then subscribe to **new** live events. A completed run's output remains in the snapshot. |
| Same-session concurrency | Explicit conflict; no interleaved model histories. Independent sessions share a bounded admission budget. |
| Cancellation | Cooperative signal and drain of actual work. Disconnect or stopping event reads detaches, not cancels. Cancel is not rollback. |
| Restart | Reopen and inspect history/actions. No automatic Pi continuation or business replay. |
| Interrupted effect | Executing calls/actions become `outcome_unknown` during explicit offline recovery. Completed calls are not replayed. |

Call `recoverInterrupted()` only after proving the old executor has stopped and
while holding exclusive maintenance ownership. It is not a lease timeout or a
cross-process fencing mechanism. A live old process can still make effects.
Do not treat a consumed confirmation as proof that an external effect succeeded.
If an effect occurred but its receipt did not persist, the Agent cannot establish
the result. Domain-specific reconciliation may inspect an existing result or a
truly stable business idempotency key, but this version supplies no automatic
retry/resume path and no cross-resource exactly-once claim.

`close()` stops admission, requests cancellation, and drains owned work before
resolving. It does not close borrowed storage, DBs, or a RunningApp. Uncooperative
tools/providers can delay shutdown; the runtime cannot safely kill them.
The host owns retention/backups and any pruning of stored history; this version
does not silently expire history or promise indefinite replay.
`maxSnapshotBytes` bounds aggregate session snapshots (default 16 MiB); oversized snapshots
return `snapshot-budget`. History and run/context/output budgets are separate.

## Pi and extensions

Both Pi packages are pinned to **1.1.0**, MIT. Their published artifacts and
official release source were checked; the relevant offline Agent/tool/abort paths
were exercised under Bun 1.4.2. Pi supplies provider calls, conversation/tool
iteration, transcript processing, and cooperative abort. This package adds host
identity/scope, explicit Lenso exposure, bounded run/session/event lifecycle,
durable action consumption and execution facts.

`examples/provider-config.ts` demonstrates explicit real-provider configuration
with placeholders. Credentials stay in the trusted provider callback, not
instructions, tools, messages, diagnostic output, or task payloads. Unknown usage
is `null`, not fabricated zero. Usage is observation, not billing.

Advanced Pi options are explicit; there is no mutable Agent or arbitrary tool
replacement exposed to plugins. Extensions cannot satisfy a confirmation or
change the execution gateway. Context/results/model output cannot enlarge the
host-selected catalog or service authorization.
`beforeRun` hooks run in declaration order and fail closed before model/tools.
`afterRun` hooks observe the persisted terminal state in order; their failures
emit safe errors without changing that outcome. Both receive only safe facts
and a signal, and both participate in shutdown drain.

Use `secrets()` to identify additional host credentials for redaction, and avoid
putting credentials in business data at all. Recognized credential keys and
known values are rejected in tool inputs and scrubbed from output. Arbitrary
unknown secrets hidden in prose cannot be detected reliably. Do not log raw
model/provider errors, evidence, requests, or hook data.
Provider callback keys are additionally held in a transient shared redaction
set for events, messages, gateway receipts, and snapshots; it is cleared on close.
Declare credentials upfront through `secrets()` if they could occur in user input
before the first provider request.

There is no `pi-coding-agent`, `pi-durable`, second runtime, queue, RAG, coding
tools, SQL console, unbounded URL fetch, terminal, filesystem, or Git tool.
Pi Durable and Cloudflare harnesses are separate execution models; a successful
Bun run does not establish Workers recovery semantics.

## Console owner: minimal wiring

Keep the existing Console Auth/session and listener owners. Install the Agent
plugin with exact dependencies, derive current subject/scope/verified target
from the trusted Auth boundary, and bind only selected business Operations.
Derive each business audience's actor through Auth, not by casting a Console
actor. Recheck access after catalog/proposal and inside the business service.

Use the Console-oriented profile in `examples/console-profile.ts` as an explicit
tool/instruction selection, not UI authority. Page context is an untrusted hint.
Mount `createAgentFetchHandler` before the existing host handler's fallthrough,
or call the async service directly from the host's existing oRPC routes.
The Fetch authenticator must enforce the host's Origin/Host/CSRF policy for
credential-bearing writes. There is no second listener or automatic mount.

Expose session/run snapshots, live events, action confirmation/cancellation, and
run cancellation. The host chooses browser delivery and calls `waitUntil` or its
existing lifetime owner for independent work. Existing Console finite Manage
transport is not an Agent event replay API. No navigation, chat, layout, shared
route, Realtime, or MCP changes are required here. There is no invented log query
interface. Tasks jobs should be exposed through selected real submit/query
Operations; Agent does not implement another job queue.

Fetch defaults to `/agent`: POST `sessions`, GET `sessions/:id`, POST
`sessions/:id/runs`, GET `runs/:id` and `runs/:id/events`, POST `runs/:id/cancel`,
POST `actions/:id/confirm` with `{digest}`, and POST `actions/:id/cancel`.
Expired actions return 410; consumed/unknown/conflicting confirmations return
409 with a safe code. No request can replace an action's target or arguments.

Optional `audit` receives only correlated security facts and is checked before
effects. It can delegate to host Audit; an in-memory Audit receipt is not the
Agent's persisted execution receipt. Host Limits, logger/OTel, and Tasks remain
their existing owners. No global SDK, production credentials, or quota is created.

## Local artifacts and checks

The attached Lenso main source currently has newer Core/Engine/Manage contracts
than its npm artifacts. `vendor/source.json` records the exact source revision,
build order, and SHA-256 values of the local tarballs. They are reproducible
integration snapshots, **not claims of published versions**. Root overrides
ensure transitive dependencies use those same snapshots. There are no deep
source imports or personal-machine absolute paths in the package manifest.

`check:consumer` packs the built package and installs it into a separate consumer
with explicit snapshot overrides. Consumers must use the same verified framework
artifacts until corresponding registry releases are available. Do not publish
this private candidate as if those dependencies already existed on npm.

The tests target concrete failures: schema/identity/instance isolation,
permission withdrawal, action tampering/version/expiration/consumption,
session/concurrency budgets, stream detachment, actual cancellation/shutdown,
SQLite restart, unknown effects, and secret-safe output. Live paid providers,
production Console integration, multi-host durable execution, and deployment are
not validated by the offline suite.
