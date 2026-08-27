# ADR 0038: Configure discovered Plugin Modules with adjacent sidecars

## Status

Accepted.

## Context

A Plugin package is immutable distribution input. Repacking it for every local
resource limit is awkward, while putting a generic Module overlay in
`lenso.local.toml` turns one small user-settings file into a second App
Definition. Configuration also belongs to Module Instances, not to a parallel
Plugin runtime abstraction.

The configuration path must preserve the useful invariant: the Kernel starts
from one complete, Schema-valid, immutable Plan. It must not recreate publisher
signatures, trust stores, revocation lists, or a second configuration service.

## Decision

An immediate Bundle child of `plugins/` may have one adjacent TOML sidecar with
the same stem:

```text
plugins/
  example.lenso-plugin
  example.config.toml
```

Directory Bundles use the same rule: `plugins/example/` pairs with
`plugins/example.config.toml`. The sidecar contains Module contribution IDs:

```toml
[modules.code-mode-tools]
max_instructions = 500000
```

There is no Plugin configuration object at runtime. During discovery the Host:

1. pairs the sidecar with exactly one Bundle;
2. rejects unknown or unselected Module contributions;
3. merges each patch over the exact Host Profile default;
4. permits only a conservative authority reduction: existing object fields,
   numbers no greater than their defaults, array subsets, and otherwise equal
   scalar values;
5. validates the complete result against the Module's exact package-owned JSON
   Schema; and
6. records both the canonical patch and complete Module configuration in the
   transient Active Set and Plugin lock before resolving a Generation.

The complete configuration therefore participates in Desired State,
Generation, recovery, and provenance digests. A sidecar edit wakes the existing
recursive watcher and stages a new Generation through the Ready Gate. It never
mutates a running Generation.

Sidecars are closed TOML, UTF-8 regular files no larger than 256 KiB. TOML
datetimes are rejected so configuration has one portable JSON value model.
Orphan sidecars, symlinks, malformed documents, and invalid patches become
individual Plugin problems; other valid Bundles can still load.

The sidecar does not broaden automatic admission. A Bundle must independently
match the Host's existing drop-in policy. Fields are configurable only when the
Host has registered that exact Module Schema and default. Stateless Modules use
no sidecar.

Static App Modules remain reviewed App Composition. Their future authoring
shape is a `configuration_file` reference under `config/modules/`, resolved and
Schema-validated by the App authoring layer. That source concern is deliberately
separate from discovered Plugin sidecars and does not introduce a Plugin
runtime configuration type.

## Consequences

- users can keep an immutable Bundle and a small local configuration beside it;
- Module Instance configuration remains the only runtime configuration model;
- local edits cannot silently widen Host-reviewed authority;
- exact configuration is reproducible across durable Generation recovery;
- removing a Bundle leaves an orphan sidecar problem instead of silently
  applying it to another Release; and
- publisher identity, signing, trust databases, and revocation machinery remain
  outside this local drop-in workflow.

## Proof

Unit tests prove partial patches become complete canonical Module
configurations, unknown fields and expanded limits fail, Module Schema limits
still apply, TOML datetimes fail, and Active Set closure reconstructs the exact
sidecar result. Source integration tests prove an orphan sidecar is reported
without blocking another Bundle and an authority-expanding sidecar is
quarantined.
