# Lenso Agent Tool SDK 0.3.3

Native Tool Provider methods may return `PluginResult<ExecuteResponse, ExecuteError>` to preserve downstream Runtime failures separately from business errors. Existing methods returning `Result<ExecuteResponse, ExecuteError>` remain compatible.

Forward the supplied context through generated `*_with_context` clients to retain cancellation, request identity and deadlines. Context forwarding does not establish end-user identity or grant permissions. The portable facade rejects native `PluginResult` signatures explicitly.

The SDK and macro authoring tests cover both result forms, Runtime failures, business rejection and context preservation. Independent Auth Account Admin acceptance uses a real PostgreSQL-backed provider to verify operator-authorized queries and mutations, session revocation, denied access and restart persistence.

Packages: `lenso-agent-tool-sdk-macros` 0.3.3, followed by `lenso-agent-tool-sdk` 0.3.3. This release does not change the TypeScript SDK or npm Agent launcher, enable Account Admin tools by default, or add end-user delegation.
