# ADR 0118: Host-authorized session-free completion

Status: Accepted for the coordinated local implementation; delivery pending review.

An optional Agent Web bridge exposes quote, bounded completion and owner-scoped
cancel under `/api/console/v1/agent/plugin-ai`. Its default authority is absent;
loopback/local data-plane access alone does not enable it. The standalone Host
may select an existing operators issuer/public-key file explicitly. No signing
key or shared user identity is created. Signed user assertions must pass the
configured issuer/key, validity, one-hour maximum TTL and exact
`lenso.agent.model@4/complete` audience. Request fields never supply identity.
Equal subjects under another issuer remain unauthorized. Assertion material is
excluded from runtime command debug output.

Every quote, completion and cancel also passes the existing Host control seam.
The private Host control credential is not accepted from model/plugin request
arguments or returned in results. An assertion alone cannot bypass the Console
adapter's budget reservation. Standalone enablement requires its already
configured control credential and a loopback listener; absent control fails closed.

The Host captures the immutable Generation's existing Model binding, catalog
input ceiling and exact provider Instance. Completion reuses the typed Model
stream and carries the assertion; it does not lease a Turn, Session or Tools.
Unknown input ceilings, missing usage, tool calls or changed quoted Generation/ceiling
fail visibly. Output is bounded, requests time out, and cancellation reaches the
native stream. There is no retry or alternate model. No capability ID changes.

Console owns the separately optional capability adapter, caller/project policy,
pricing revision and durable quota ledger. Kernel supplies caller Instance;
Console verifies its own operators assertion/audience and selects the one
Host-admitted project for that caller/user. The bridge does not trust unsigned
project or caller fields. It has no filesystem/tool effect or history writer.
Model providers and Auth remain final owners of their keys and grants.

The quote is provider catalog evidence for a hard ceiling, not vendor token
estimation or a billing receipt. Console reserves the full ceiling before a
completion and records exact binding/Generation with usage. Actual provider-side
model substitution cannot be inferred from this existing Model wire contract;
the selected trusted Model provider must honor the exact request. Unknown pricing
or unbounded usage cannot enter the paid path. Live parent-credential revocation
and scoped multi-round runs are not provided by this short-lived assertion bridge.

Proof: an actual Console Kernel consumer calls a separate local Agent fixture
process. Foreign issuer/audience and spoofed caller/project attempts are rejected;
cancel/failure retains budget, and Session lists before/after remain empty.
