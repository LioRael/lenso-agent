# Business Plugin Tool integration

Status: native Tool authoring supports downstream Domain and Runtime outcomes.
Authenticated business reads/writes are not yet integrated or released.

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

## Business identity prerequisite

The current Agent source has no Auth ActorAssertion integration. Model login,
the Console control token, a caller Instance, a Session ID and an approval mode
are not evidence of an authenticated Projects user.

The existing Projects provider requires an Auth-issued assertion, checks the
caller Instance, Organization membership and Access Control, and then enforces
Team visibility. Its Web consumer authenticates credential evidence before
attaching an assertion. A Tool cannot replace that path with a configured user
ID, a model-provided actor field, or an administrative control credential.

Before exposing real Projects tools, the owning Auth/Agent boundary needs an
explicit delegated-user contract specifying:

- which authenticated ingress establishes the actor and session association;
- how a Turn obtains an audience-bound assertion without exposing credentials
  or signing material in model inputs, histories or Tool outputs;
- exact provider/operation scope, expiry, revocation and failure outcomes;
- preservation through Tool Hooks and any App Agent forwarding; and
- denial when identity is missing, expired, revoked or inappropriate for the
  target, independent of Agent approval mode.

This work belongs to Auth/Agent integration. Console Workspace routing and the
cross-App service Connector remain owned by the separate Console task. No new
universal Capability proxy is introduced here.

## First real business proof

After the identity prerequisite is implemented, use the existing Projects
provider for `get_issue` and a state change through `update_issue`. The latter
is a full aggregate update: preserve other fields and the observed
`expected_revision`. Read the real Team workflow catalog. A conflict is a
business outcome and must not silently repeat or overwrite the user's edit.

Acceptance includes authorized query and mutation, missing/denied actor,
private-Team denial, expired assertion, revision conflict, provider outage,
cancellation and no automatic replay after an ambiguous mutation. Confirm the
durable record and prove denied calls do not change it. Test through generated
clients and a resolved runtime composition, not only direct provider methods.

## Evidence and limits of this change

The SDK authoring tests exercise generated dispatch for business rejection,
Runtime failure, successful JSON text and metadata, cancellation-token state,
request ID, caller Instance and deadline. Existing text Tool tests cover
backward compatibility. The macro test covers portable rejection.

These tests use a fixture, not an authenticated business provider. They do not
prove signed identity propagation, real Capability dispatch, durable writes,
streaming, or end-to-end cancellation. The query/mutation acceptance above
remains outstanding until the identity contract and actual consumer exist.
