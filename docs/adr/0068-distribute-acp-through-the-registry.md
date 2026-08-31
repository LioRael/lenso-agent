# ADR-0068: Distribute ACP through the Registry

Status: Accepted

## Context

ADR-0067 added a stable ACP protocol-v1 process boundary. The original roadmap
then named first-party VS Code and Zed packages, but the supported editor
distribution surfaces have changed. Zed deprecates Agent Server extensions in
favor of the ACP Registry. VS Code currently exposes MCP and customization
extension points but no public API that registers a third-party ACP Agent.

An editor package must not duplicate the Agent runtime, describe ACP as MCP,
or carry a second App configuration. Registry admission also requires
versioned downloadable artifacts with integrity digests and at least one real
ACP authentication method.

## Decision

- Zed distribution targets the ACP Registry, not the deprecated Zed extension
  format. Before publication, each declared platform archive is built from the
  `lenso-agent-acp` binary, assigned an exact version, and pinned by SHA-256.
- The release packager creates one deterministic archive and checksum from an
  already-built target binary. `render-acp-registry-entry.py` fails closed
  on invalid versions, latest aliases, missing archive/checksum pairs, malformed
  digests, and an empty platform set.
- The Registry manifest starts `lenso-agent-acp` without a client-authored
  Profile or Plan. Agent Home and Profile selection remain native Lenso
  configuration.
- ACP initialization advertises one Agent Auth method for direct ChatGPT
  authentication. `authenticate` owns the existing browser OAuth flow at the
  Host surface and writes through the existing Auth Plugin credential owner.
- No first-party VS Code package is published until VS Code exposes a supported
  ACP registration API or Lenso explicitly decides to own and maintain a full
  ACP client extension. A third-party client can start the same binary, but it
  is not branded as first-party integration.

## Consequences

- Zed receives Lenso through its current supported install path and verifies
  the downloaded bytes before executing them.
- Release publication and the external Registry PR remain explicit delivery
  operations; a local template cannot claim that nonexistent release URLs are
  installable.
- Authentication remains usable after Host startup because the direct Auth
  Provider reads its credential owner for each access request.
- VS Code support is represented truthfully as a client availability gap rather
  than an MCP configuration or an obsolete package.

## Proof

ACP integration tests inspect the advertised Agent Auth method while retaining
the session, approval, cancellation, and Generation provenance transcript.
Packaging validation builds the native binary, reproduces byte-identical
archives, verifies the SHA-256 record, renders a schema-shaped Registry entry,
and rejects incomplete or unversioned inputs.
