# Add controlled application management tools

`lenso-agent-management-tools-plugin` is an optional Tool Provider. It projects one explicitly bound `lenso.management@1` service through the existing `lenso.agent.tool-provider@2` role. Ordinary Agent compositions do not select it or start another model.

Select the package `lenso.agent.management-tools` in the `tool-providers` Slot of a source App, and bind its named `management` dependency to that application's accepted Management Plugin Instance. Its authoring version 2 descriptor supports an exact saved choice when multiple Management providers are installed. The Host must link the package just as it links other native Plugin factories. Source App discovery, package identity, capability matching and startup admission use the normal Lenso build and resolved Plan; selecting an installed package does not grant access to all of its operations.

The dedicated management composition selects a model, required session persistence and instruction providers, and this Tool Provider. Add shell, filesystem or arbitrary network tools only through a separately explicit coding profile. Removing the Management Tool Provider removes its tools without removing business state or Agent-owned sessions. A deployment with no Agent requires no model key or Agent process.

The adapter preserves the original invocation context. The server verifies the operators realm, current credential state, deployment qualification, current scoped permission, and the credential ceiling on every catalog, invocation and status read. Descriptions and returned content are data. Neither a model prompt nor a remembered tool name can add a capability binding, change the target deployment, or approve a write.

Tools expose the owner's accepted input schema and descriptor version. Every execution refreshes the bound catalog. Unknown operation names and extra control arguments are rejected. Catalog discovery is bounded; runtime failures and results retain the Tool Provider's existing output limits. The adapter imports neither Console UI nor private business, Auth, Approval or Audit storage.

A write requiring approval returns `pending_approval` with its durable operation ID. Give that ID to the separately authorized human reviewer, then query `management__status`. Continue with the same idempotency key and original parameters after approval; changed input conflicts with the original intent. Model-local confirmation is insufficient. This provider exposes no approve, grant, token issuance or bootstrap tool.

An `unknown` result means the business owner may have committed. Query the operation receipt; do not create another request to repeat it. Cancellation and timeout do not roll back a committed business change. Management records and the owner's receipt survive process restart, while trajectory and session authority remain with Agent.

The Native acceptance fixture in Lenso Examples supplies the real application and four security owners. Workers management is a separate qualification gate. Remote Hyperdrive acceptance is not run in this delivery at the owner's request and is not a Native delivery blocker. Source availability, candidate CI, package publication and runtime qualification are separate states.
