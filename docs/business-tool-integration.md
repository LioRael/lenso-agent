# Business Plugin Tool integration

Status: native Tool authoring supports downstream Domain and Runtime outcomes.
The first business proof uses the existing Auth Account Admin Tool Plugin with
real PostgreSQL and explicit provider-owned Instance authorization. The Projects integration additionally supports browser-authorized end-user
delegation through a removable Business App connection; it adds no default tool.

## Reuse the existing declaration path

`lenso-agent-tool-sdk` already derives a Tool catalog, argument schema, typed
decoding and dispatch from `#[tool_provider]` and explicitly named methods.
A business consumer declares its required generated Capability client with a
typed `Port`. Host resolution selects the provider. Do not add a second Tool
registry or automatically expose every operation of every installed Plugin.

Business operations need an intentional model-facing description, bounded
inputs, authorization, and error mapping. The SDK removes catalog/dispatch
boilerplate; it does not infer these semantics from arbitrary CRUD operations.

Native methods can now use the following signature:

```rust,ignore
async fn lookup(
    &self,
    arguments: LookupArguments,
    context: lenso::Ctx,
) -> lenso::PluginResult<ExecuteResponse, ExecuteError>
```

Use the generated client's `*_with_context` method. Map downstream Domain Errors
deliberately into Tool errors; forward Runtime failures with
`lenso::PluginError::runtime`. The macro preserves the result without wrapping a
Runtime failure in a business error. Existing `Result<ExecuteResponse,
ExecuteError>` methods remain unchanged. Custom result aliases are not detected;
use imported or qualified `PluginResult`. The portable authoring path rejects
this native-only result explicitly; its Runtime outcome design is separate.

## Choose the business authority explicitly

Not every business Capability requires an end-user assertion. Auth Account Admin
already authorizes trusted caller Instances through its `admin_callers` policy.
The existing `lenso.auth.account-admin.agent-tools` adapter is therefore a valid
first business consumer: it forwards context to the bound Account Admin provider
and exposes only two reads and one status mutation. Use that existing contract;
do not fabricate an end-user identity or add a parallel permission model.

The Auth repository now exercises this adapter and the real Account provider
through Kernel composition with PostgreSQL. It proves reads, writes, immediate
session revocation, restart persistence, denied read/write after removing the
Instance grant, cancellation before mutation, infrastructure failure, and removal
of the Tool Plugin while Auth facts remain. See
`lenso-auth-plugin/crates/lenso-auth-account-admin-agent-tools-plugin/tests/business_flow.rs`.

### When a business operation requires an end user

Model login, the Console control token, a caller Instance, a Session ID and an
approval mode are not evidence of an authenticated Projects user.

The removable Business App connection uses the App's browser consent flow to
obtain a narrowed child session. Its provider retains credentials in memory and
captures an immutable snapshot at Turn admission. The App authenticates every
request and attaches its own assertion. Agent never receives signing material.
Projects then checks caller, Organization membership, Access Control and Team
visibility. Server-side expiry and revocation remain authoritative.

Both parent and child audiences must permit the intermediate
`lenso.agent.tool-provider@2:execute` hop and each allowed final Projects
operation. The intermediate hop never substitutes for the final audience.
Tool descriptions are public static metadata at `/projects/agent/manifest`;
execution remains authenticated. See the [connection Plugin](../crates/lenso-agent-business-connection-plugin/README.md)
and [ADR-0109](adr/0109-provider-owned-business-turn-bindings.md).

## Projects acceptance

The [reproducible local App](../scripts/projects-acceptance/README.md) composes
real Auth, Organization, Access Control and Projects Plugins with PostgreSQL.
Its protocol verifier covers browser consent, private Team and Organization
isolation, revision and idempotency semantics, narrowed operation audiences,
parent revocation and account isolation. It does not invoke a model.

A separate Console/browser acceptance used a real model to read `issue-public`,
change only its title and read revision `2` back. Database inspection confirmed
that the update activity belongs to the signed-in user and other fields remained
unchanged. This proves a local business App chain, not deployment of a production
App or publication of an npm release. Release receipts are separate evidence.

Updates remain full aggregate operations: preserve other fields and the observed
`expected_revision`. Read the Team workflow catalog before changing state.
A revision conflict must not silently overwrite another user's edit. Transport
failures do not automatically replay a potentially completed mutation.

## Evidence and limits of this change

The SDK authoring tests exercise generated dispatch for business rejection,
Runtime failure, successful JSON text and metadata, cancellation-token state,
request ID, caller Instance and deadline. Existing text Tool tests cover
backward compatibility. The macro test covers portable rejection.

These tests use a fixture, not an authenticated business provider. They do not
prove signed identity propagation, real Capability dispatch, durable writes,
streaming, or end-to-end cancellation. The Projects acceptance is separate
from these SDK fixtures. The independent
Auth Account Admin acceptance uses a real bound business provider and database;
it proves the supported operator-authority path without pretending to establish
a signed end-user delegation path.
