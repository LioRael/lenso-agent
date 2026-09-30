# ADR 0117: Bind remote Management to an Auth-issued task

Status: Accepted

## Context

An Agent in another process must preserve the user and current parent credential
while reducing authority to one task and Agent Session. A browser Cookie,
external MCP bearer, shared administrator identity, or model-selected URL cannot
serve as that delegation boundary.

## Decision

Auth Account owns issuance, secret custody at issuance, signed task binding,
expiry and parent revocation. Its scoped child credential is opaque and short
lived. The selected Management application authenticates it on every request,
verifies the signed task/Session/caller binding, and intersects current parent
ceilings, qualification and RBAC with the signed operation/resource ceiling.

The removable `lenso.agent.management-connection` Plugin owns one Host-selected
HTTPS origin, deployment and private child credential file. It exposes only the
neutral Management catalog, invoke and status Capability. It has no generic
network Tool and never accepts identity, credentials or a target from model
arguments. Redirects and automatic retries are disabled. Uncertain invocation
transport returns `unknown`; it does not replay the intent.

The Host attaches `AgentTaskBinding` to each Turn. Loop rejects a different or
missing Session before opening it. The remote connection requires the exact
binding. Explicit optional ManagementTools bootstrap labels permit its selected
catalog to load before a Turn exists, while execution still requires the Turn's
binding. Those labels confer no authority: Auth and Management remain the final
owners of every catalog and command decision.

The deterministic task Host uses source-generated Plugin Descriptors and the
existing Core Plugin Root loader/resolver. It selects only fixture Model, Loop,
durable Session, instructions and Management Tool Provider plus the Loop's
bounded context support. No coding, filesystem or arbitrary network Tool is
selected. The ordinary Agent Foundation and existing coding profiles retain
their defaults.

## Consequences

Session events and trajectories remain Agent-owned. Credentials stay outside
model inputs, tool output and receipts. Restart resumes the same bound Session
and may query an accepted operation reference. A transport outcome without an
accepted reference requires operator reconciliation under the original intent
identity; it is never evidence that a write failed.

Native scoped delegation is supported by the owner implementation. Workers
scoped issuance remains explicitly unsupported until it is qualified. A real
model is an optional interaction probe and is not a security test dependency.
