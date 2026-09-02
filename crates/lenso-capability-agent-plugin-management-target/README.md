# Agent Plugin Management Target Capability

`lenso.agent.plugin-management-target@1` is a Host-private role that lets the Console Agent address one exact Agent's Plugin management authority.

The Console Plugin Tools are the consumer. The Agent Host bridge is the provider. Each inspection, proposal, publication, and selection request carries an explicit `agent_id`, and each successful response repeats it so callers can reject mismatched receipts. The provider may route to the local Console authority or to an injected App Agent adapter; an absent or unsupported target fails closed and never falls back to another authority.

This Capability does not own Agent discovery, Plugin configuration facts, authority selection, HTTP transport, runtime activation, or UI presentation. Those remain with the Agent catalog, the target Agent's configuration and selection authorities, Host adapters, Runtime, and Console respectively.

The role is non-portable and does not cross lanes. It exists to keep target routing out of the authority contracts, whose provider remains the single owner of one Agent's configuration state.
