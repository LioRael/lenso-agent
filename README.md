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
bun run check:consumer:ordinary
bun run check:consumer:ordinary --npm
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

The service exposes `createSession`, `readSession`, `readMessages`, `startRun`,
`getRun`, `events`, `watchRun`, `cancelRun`, `confirmAction`, `cancelAction`,
`recoverInterrupted`, and `close`.
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

Terminal action outcomes are also source-labelled conversation facts, committed
in the same transaction as the action and call receipt. The deterministic
`action-outcome:<actionId>` message ID prevents duplicate projection across
confirmation, reads, and restart. Facts use existing `completed`, `rejected`,
`outcome_unknown`, `cancelled`, and `expired` states; an invocation failure with
uncertain effects remains unknown, not a fabricated failed/successful receipt.
Cancellation of a pending action is not rollback of an already-entered effect.

The next **user-started** run reads these safe facts as ordinary reference data,
not executable tool calls or system instructions. Confirmation does not start
another model request. Facts contain correlation and bounded, redacted receipts,
not original executable arguments. Existing terminal actions without a fact are
backfilled on `readSession` and before run admission/context assembly,
deterministically inside an authorized, bounded session transaction.
Outcome correlation metadata has its own 32 KiB ceiling; optional receipts retain
`maxOutputBytes`. Oversized tool/model labels are explicitly omitted, so a small
receipt budget cannot prevent cancellation or interrupted recovery.
Reads and context loading revalidate current host identity against the exact
subject/scope/application/instance binding. The host identity resolver remains
responsible for current session read permission; this package does not invent a
separate business-result authorization policy.

## Storage, events, and recovery

```ts
import { Database } from "bun:sqlite";
import { createSqliteStore, migrateAgentDatabase } from "@lenso/agent/sqlite";

const db = new Database("agent.sqlite");
migrateAgentDatabase(db); // explicit host-controlled installation step
const store = createSqliteStore(db); // borrowed; close does not close db
```

Migrations use `lenso_agent_*` tables and their own schema version. They do not
change host `user_version`, busy timeout, or other pragmas. Transaction callbacks
are synchronous and atomic. Separate SQLite handles use `BEGIN IMMEDIATE`;
`SQLITE_BUSY` propagates without hidden retry. The host owns database connection
and busy-timeout policy. Default construction does not create tables.

| Capability | Guarantee and limit |
| --- | --- |
| History | Accepted user messages persist atomically with run admission, including context-loading failures. Safe finalized outputs, run/call states, actions, and results persist. |
| Live events | Ordered IDs, sequence, timestamps, run/session/call/action correlation. Bounded subscriber buffers. No persisted delta log or historical event replay. |
| Reconnect | `watchRun` registers bounded live delivery **before** reading the authoritative snapshot and its sequence watermark. This is snapshot + live, not a recoverable delta log. |
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
return `snapshot-budget`. Stored UTF-8 byte metadata is checked in the authorized
transaction **before** materializing aggregate records; redacted output is checked
again. History and run/context/output budgets are separate.

```ts
const watch = await agent.watchRun(context, runId, { signal });
renderSnapshot(watch.snapshot);
try {
  for await (const event of watch.events) {
    // Already deduplicated and newer than watch.sequence, for this exact run.
    applyLiveEvent(event);
  }
} catch (error) {
  // resnapshot-required is also emitted after every live terminal notification.
  // Replace the transcript from the final persisted facts, not streamed fragments.
  renderSnapshot(await agent.readSession(context, watch.run.sessionId));
  // If still running, attach a new watch with fresh authentication.
} finally {
  await watch.close(); // detaches only, never cancels the server run
}
```

Run event sequences are local to one run's in-memory event source, not persisted
database revisions. The snapshot and watermark are captured synchronously after
subscription; buffered events at or below the watermark are discarded. This does
not replay transient text or preserve an in-flight prefix: only finalized
messages are durable. Duplicate events are ignored, and a terminal snapshot
cannot regress to `running`. A sequence gap, overflow, wrong correlation, or
live stream closure requires a fresh snapshot. Every live terminal notification
is followed by `resnapshot-required`; fetch the authoritative final snapshot and
**replace** the transcript, rather than appending it to incomplete streamed text.
`run_interrupted` can mean its receipt is unavailable and storage still says
`running`; offline exclusive recovery, not retry, is then required. An
already-terminal initial snapshot needs no live stream. Another executor's
running marker without a local live source cannot promise continuity.

`readMessages(context, sessionId, {limit, after, through})` provides bounded UI
pages in stable insertion order (`limit` 1..1000, default 100). Continue using the
returned `cursor` as `after` and the original `through` watermark. Later inserts
are excluded; an updated record retains its insertion position and each page
sees its currently committed value. Pages are not a multi-request MVCC snapshot.
Public cursors are session-scoped message IDs, not global insertion counters;
`""` represents an empty bound. A foreign or missing bound is rejected alike.
This API pages already-recorded messages without loading the action ledger.
For a legacy session, perform its bounded `readSession` backfill before paging
the newly projected facts. New confirmations already commit their facts directly.
Authorization precedes range reads, including when reusing a cursor. UI pagination
does **not** truncate model context: admission and context assembly still require
the complete necessary history, including outcome facts, within `maxHistoryBytes`.
Over-budget history is rejected explicitly rather than dropping dialogue or
inventing partial tool protocol pairs.

Custom stores must implement `count`, `bytes`, and `page` alongside `get/list/put`;
all methods see writes within the same synchronous transaction. Memory storage
uses a rollback-safe write set and detached read/write values. SQLite v2 adds
query and byte metadata; explicitly run the complete `migrateAgentDatabase` on an
existing v1 database before opening it with this version. The 002 SQL file alone
is only its DDL component, not a complete upgrade. Agent tables must be written
through `StoreTransaction.put`, not direct host SQL that bypasses byte metadata.
No migration runs on construction.

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

GET `runs/:id/watch` streams an initial SSE `snapshot` frame containing
`{snapshot, run, sequence}`, then newer live events. A `resnapshot_required`
frame means the client must reconnect and replace state from a fresh snapshot.
`Last-Event-ID` does not provide persisted replay, on either stream endpoint.
The older `/events` route is live-only and must not be used with a separate
snapshot-first request as a gap-free reconnect protocol.

Optional `audit` receives only correlated security facts and is checked before
effects. It can delegate to host Audit; an in-memory Audit receipt is not the
Agent's persisted execution receipt. Host Limits, logger/OTel, and Tasks remain
their existing owners. No global SDK, production credentials, or quota is created.

## Local artifacts and checks

Framework dependencies use real registry versions: Core `0.3.0`, Engine `0.4.0`,
Manage `0.4.0`, and development/example Auth `0.3.1`. On 2026-10-09 their npm
tarballs were verified byte-for-byte against the retained vendor artifacts.
`vendor/source.json` remains their build provenance and checksum record, not a
claim that they are still unpublished. The package manifest has no source paths,
vendor file dependencies, or author overrides. Engine remains a runtime dependency
because both safety/diagnostics and the gateway use it; `/manage` and `/plugin`
are separate imports, not a promise of an Engine-free installation.

`check:consumer` deliberately installs copied framework artifacts with explicit
overrides for **special framework integration**. `check:consumer:ordinary` instead
packs the unchanged candidate, installs only its registry dependency graph in an
independent Bun consumer, and never copies vendor artifacts or adds overrides.
Pass `--npm` for an independent npm install and `npm ci` check. Both modes import
all public entries, typecheck, and run the offline Pi/Auth/Manage workflow against
the packed entries rather than author source. Auth/Pi and checking tools are
explicit fixture dependencies, not hidden replacements for Agent dependencies.
Ordinary checks first install a minimal consumer with **only** the candidate
dependency and import all entries, before adding the explicit offline fixture.
Consumer TypeScript uses the baseline `skipLibCheck: true`; add
`--check-dependency-types` to inspect dependencies too. That stricter check was
attempted and failed on published dependency declarations: `@google/genai`
references its absent optional MCP SDK, and `gaxios`' fetch declaration conflicts
with Bun's required `fetch.preconnect`. No overrides, fake versions, or extra
provider dependencies were injected to conceal those errors. Normal candidate
installation, public-API typechecking, and the offline workflow passed; full
dependency-declaration compatibility remains an upstream follow-up.
The errors were reproduced again after the naming decision. Pi `1.1.0` is still
its latest published version: its public Google type declarations pull in
`api/google-shared`, then `@google/genai`, the optional MCP declaration, and
`google-auth-library`/gaxios. A corrected Pi public-type boundary or corrected
upstream declarations must be available before this gate can pass. This package
does not patch installed dependencies, copy Pi's contracts, invent a release
version, or require consumer overrides as a workaround.

**The embedded library will retain `@lenso/agent`.** The owner confirmed this
name; no separate library name is planned. npm currently assigns
`@lenso/agent@latest` (`1.20.0`) to Console's native launcher with `lenso-agent`
Web/CLI/ACP commands, not this embedded library's API. The npm owner is `liorael`;
this is not publishing authorization. `@lenso/agent@0.1.0` is not published.
Ordinary local candidate checks use a supplied tarball; running
`npm install @lenso/agent` today does not install this library. Release versioning,
launcher compatibility/migration, and a future `latest` transition need separate
approval before publication. The naming decision does not authorize renaming or
deprecating the old launcher, adding a replacement CLI, or publishing either
product. This task changes no versions or dist-tags and retains `private`.

Read-only Console audit also found its native release cohort still points to
`LioRael/lenso-agent` for `v0.1.13`, whose download URLs return 404. The matching
release and pinned asset hashes exist in `LioRael/lenso-agent-rust`. Fixing that
Console-owned download wiring is a separate follow-up; it does not establish
whether already-published native binaries work. No Console, Relay, CLI, ACP, or
production files were changed here.
The follow-up audit checked all ten old URLs (404), all ten canonical URLs (200),
and all pinned hashes against both release digests and `SHA256SUMS`. The minimal
Console change is only `tooling/distribution/agent-release.json`'s repository
from `LioRael/lenso-agent` to `LioRael/lenso-agent-rust`, preserving the exact
`v0.1.13` tag and hashes. Do not substitute `releases/latest`. Console must be
attached before editing; its nine launcher/optional-runtime fixture tests passed,
but no downloaded native executable, production Console, or ACP session was run.

The tests target concrete failures: schema/identity/instance isolation,
permission withdrawal, action tampering/version/expiration/consumption,
session/concurrency budgets, stream detachment, actual cancellation/shutdown,
SQLite restart, unknown effects, and secret-safe output. Live paid providers,
production Console integration, multi-host durable execution, and deployment are
not validated by the offline suite.

### Provider transport validation boundary

`bun test tests/provider-transport.test.ts` exercises the real Pi loop and its
published OpenAI Completions adapter with OpenAI SDK `7.19.0`, against a temporary
HTTP/SSE server bound to `127.0.0.1`. It uses only a fixed fake credential, rejects
non-loopback fetch targets, and does not substitute a second execution loop.
Checks cover a parsed tool proposal, human confirmation without another HTTP
request, the receipt in the next user-started request, actual stream abort/drain,
and safe handling of a provider HTTP error.

This fills the local adapter/SDK transport gap, not live-provider acceptance:
responses are deterministic fixtures, not model-generated output. Paid or public
providers, production credentials/data, Console integration, native Web/CLI/ACP,
other provider protocols, and multi-process fencing remain separate checks.

## Measured storage boundary

Reproduce locally with `bun scripts/storage-benchmark.ts --legacy` and
`bun scripts/storage-benchmark.ts`. The legacy mode preserves the old algorithms,
not a historical checkout. The original implementation was also measured before
editing. Environment: Bun 1.4.2, macOS arm64, Apple M2 Pro; SQLite `:memory:`.
Data: 0/1k/10k unrelated sessions, five records each (0/5k/50k rows), with the
target fixed at 82 records. Each operation has three warm-ups, a forced GC, then
30 measured iterations. Results below are milliseconds/operation, old → new,
from a same-machine comparison during this task:

| Store operation | 0 unrelated sessions | 1k | 10k |
| --- | --- | --- | --- |
| Memory single run | 0.1370 → 0.0032 | 6.1273 → 0.0017 | 52.6334 → 0.0011 |
| Memory atomic admission | 0.1061 → 0.0072 | 5.6974 → 0.0032 | 51.0443 → 0.0043 |
| Memory snapshot | 0.2294 → 0.0681 | 5.8992 → 0.0584 | 49.8927 → 0.0648 |
| SQLite single run | 0.0055 → 0.0052 | 0.0034 → 0.0028 | 0.0039 → 0.0040 |
| SQLite atomic admission | 0.0185 → 0.0286 | 0.2728 → 0.0136 | 2.6177 → 0.0161 |
| SQLite snapshot | 0.1275 → 0.1030 | 0.1051 → 0.1012 | 0.1272 → 0.0989 |

At 10k unrelated sessions, memory JSON parses per operation changed from
50,083 → 1 for a run read, 60,108 → 2 for admission, and 50,165 → 82 for a
snapshot. SQLite admission parses changed from 10,025 → 2, with six SQL
statement calls in both versions. Its already-local single-run read stayed at
one parse/three statements. Snapshot still parses 82 required records; budget
prechecks increase its statements from seven to twelve. SQLite byte sums scan
only the requested session's metadata.

This is a store-boundary microbenchmark, not `startRun`/`readSession` latency,
disk durability, multi-process contention, retained-memory measurement, provider
cost, or online throughput. Instrumentation and a short sample affect timing;
sub-millisecond differences, especially at zero history, are not reliable
performance claims. The evidence supports removing unrelated-history work,
not extrapolating production gains.
