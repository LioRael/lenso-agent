# Business App connection

This removable linked Plugin owns one Projects App connection. Configure an
Instance under the Agent Home Plugin Root with a clean `origin` and a display
`label`. The Host makes the Plugin available; it is not enabled by default.

The Plugin provides `lenso.agent.auth-connection@1`,
`lenso.agent.turn-binding@1` and `lenso.agent.tool-provider@2`. It requires no
other Plugin's storage or private API. Removing its Instance removes its login
and Tools without affecting other accounts or native Host Tool targets.

Console opens the App's browser consent page. The App must already have a real
user login and provide `/auth/agent/connection/begin`, `/auth/agent/authorize`
and `/auth/agent/connection/poll`. The Agent retains the polling secret and child
grant in memory; Console receives only the consent URL and non-secret state.
Restart or Generation replacement requires a new connection. No grant is saved
in Plugin configuration, Session history, a model request or Tool arguments.

At Turn admission the Host captures a provider-owned immutable credential
snapshot. A disconnected snapshot stays disconnected. Login changes and local
disconnect affect new Turns. Existing Turns retain identity until their lease
ends, or at most one hour. Cancellation releases the snapshot through a managed
task; shutdown ends all such tasks. Remote App revocation and expiry are checked
at every Tool request and remain authoritative.

Generation preparation reads public static Tool descriptions from
`GET /projects/agent/manifest` without a credential. Descriptions remain available
before login; execution still requires the admitted Turn identity. The integration uses
`POST /projects/agent/tools/execute`. HTTP ingress must map the Bearer credential
to the protocol-neutral `session` scheme. The App owns Auth authentication,
operation audiences, membership, scoped RBAC, Team visibility, revisions and
idempotency. Requests are not retried; failures that may follow a mutation never
trigger an automatic second execution. Redirects are disabled and responses are
bounded. Only HTTPS or explicit loopback HTTP origins are accepted.

Verification includes native Kernel dispatch against an HTTP test service,
account switching, revocation, cancelled/expired/unknown bindings, non-secret
status/poll projections and clean shutdown. That fixture alone does not prove a
real business login, PostgreSQL authorization, model use or npm distribution.

For a real PostgreSQL App and Console/model acceptance, use the
[Projects acceptance guide](../../scripts/projects-acceptance/README.md). Parent
and child sessions must authorize both the Tool Provider execute hop and the
exact final Projects operations; neither scope substitutes for the other.

## Daily workflow presentation

Account management exposes the configured App origin, the App-issued subject ID,
expiry and reconnect state through AuthConnection 1.2. Credentials remain private
and memory-only. A remote 401 invalidates only the exact grant used by that Turn;
a resource-level 403 does not disconnect the account. Newer account connections
are unaffected by a late rejection from an older Turn.

Successful Issue responses include `_links` derived from the configured origin
and returned organization/Issue IDs. Opening a link uses the App's own browser
session and does not grant access. The matching Projects Web surface owns the
`/projects?organization_id=...&issue=...` route. Removing this connection removes
these Tools and local account state; it does not delete Projects data.
