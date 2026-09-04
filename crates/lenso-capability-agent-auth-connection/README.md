# Auth connection

`lenso.agent.auth-connection@1` owns user-facing connection management separately
from credential access. The annotated Rust source in `src/contract.rs` owns the
contract; its Descriptor, Schemas and native projection are generated snapshots.
This new identity does not change `oauth-access` or `auth.openai-codex` consumers.

Operations: `status`, `begin`, `poll`, `cancel`, `disconnect`. Each bound provider
manages its own configured account/profile. A surface selects an explicit
Plan-bound provider, never a package name, credential path or arbitrary issuer.
`status.methods` determines which login actions can be offered. An empty list
is valid for non-interactive providers. The current Codex provider offers device
codes; its existing CLI browser login is unchanged.

Attempts are opaque, bounded and generation-local. At most one attempt is pending
per provider instance. Cancelling joins pending work before acknowledging it;
disconnect joins pending work before deleting local credentials. A successful
disconnect does not imply remote token revocation. Connected means a stored
grant is present, not that remote access has just been checked.

The Codex provider returns the existing pending device attempt on repeated
`begin`, so a settings reload can resume without starting another OAuth flow.

No operation returns access tokens, refresh tokens or credential paths. Attempt
IDs, authorization URLs and user codes are sensitive presentation data: do not
log them or persist them in conversation history. The management surface must
authenticate and authorize the user and must not expose these operations as
automatic model Tools. Browser-loopback login must only be offered when the
user can reach the provider Host's callback; device login works with remote Hosts.

The initial implementation is native-only (`portable = false`); HTTP surface
projection is a separate consumer integration, not an implicit public endpoint.
No Web routes or Console controls are shipped by this contract package itself.
