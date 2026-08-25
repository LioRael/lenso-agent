# ADR 0024: Enforce reviewed network grants through one HTTP Capability

## Status

Accepted.

## Context

ADR 0021 proved that a third-party Wasm Tool can import one Host-selected read
Capability without receiving ambient filesystem authority. Network access needs
the same explicit boundary, plus a distinction between what a publisher asks
for, what review approves, what an App permits, and what the runtime enforces.
Treating review evidence or a Manifest Permission Request as authority would let
a release expand its own reach.

## Decision

The Harness owns portable Capability `lenso.agent.http-fetch@1`, Descriptor
`1.0.0`, with one request Operation: `get`. A native
`lenso.agent.http-fetch` Module provides it. The Provider accepts only exact
canonical HTTP or HTTPS origins from App configuration, disables redirects,
rejects URL credentials, applies timeout and response-size limits, and returns
UTF-8 bodies only.

The Plugin Profile Catalog admits one package-independent Wasm Tool shape with
exactly that Capability requirement and one required `network` Permission
Request. Its scope is `{ "origins": [...] }`, containing one to eight sorted,
unique canonical origins with no path, query, fragment, or credentials. The
Host Profile fixes the import to the `http-fetch` Instance and its exact package
identity; the Bundle cannot select another Provider.

Explicit review promotes the requested scope into an immutable
`ApprovedGrant`. Active-set validation recomputes the exact expected grants.
Generation readiness then requires each approved origin to be contained by the
App-selected Provider's `allowed_origins`. The runtime derives the effective
Host grant set through the existing control plane, and the Provider enforces the
same origin boundary on every call. Install, upgrade, rollback, and removal
replace or remove grants together with the release authority.

The checked-in base `lenso.app.json` selects the Provider with an empty
allowlist, so it grants no network access until App intent explicitly names an
origin. Kernel and Wasm Adapter policy remain unchanged; they receive resolved
bindings and effective grants rather than a mutable Plugin registry.

## Consequences

- A publisher request, review evidence, App policy, and runtime enforcement are
  separate, inspectable authorities.
- Adding an origin during upgrade fails the Ready Gate unless the App already
  permits it, and the active set remains unchanged.
- Redirects cannot escape the approved origin because they are disabled.
- The standalone external example proves a real loopback GET across install,
  upgrade, rollback, and removal, and proves unauthorized origin expansion is
  rejected.
- Generic sockets, arbitrary HTTP methods, request headers or bodies, Secrets,
  process access, state, Data mounts, and custom bindings remain unsupported.

## Rejected alternatives

Giving the Wasm component WASI sockets would create broad network authority
outside the Capability contract. Trusting only the App allowlist would lose the
release-specific reviewed scope; trusting only the Permission Request would
delegate policy to the publisher. Performing origin checks only at Generation
time would leave redirects and malformed runtime URLs unenforced. A live Kernel
Plugin registry would make the immutable Plan and Generation cease to be the
authority boundary.
