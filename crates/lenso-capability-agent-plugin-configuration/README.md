# Agent Plugin Configuration Authority Capability

`lenso.agent.plugin-configuration-authority@1` is the Host-private role used by
native Agent Plugins that must inspect, propose, or publish through the exact
`PluginConfigurationAuthority` selected by their Host.

It does not own provider selection, storage, remote transport, publication
history, rollback, Generation operation tracking, or Console HTTP routes. The
Host bridge supplies one Plan-bound provider backed by the same local, SQLite,
remote, or injected custom authority used by the Agent Web control surface.

This contract is intentionally distinct from Console's cross-Agent
`lenso.agent.plugin-configuration@1`. That product Capability advertises the
broader configuration-management HTTP surface to another Agent identity,
including management, history, rollback, publication operations, and their
observable Generation outcomes.

The authority Capability is non-portable today because both its provider and
consumer are linked native Plugins. A remote configuration service changes the
authority's private storage transport; it does not cross a Lenso Execution
Adapter boundary.
