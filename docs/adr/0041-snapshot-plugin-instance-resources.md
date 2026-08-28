# ADR 0041: Snapshot Plugin Instance resources

## Status

Accepted.

## Context

Some Plugin configuration is naturally structured data rather than TOML: long
prompts, templates, scripts, certificates, and static assets. Putting those
bytes into one configuration file makes ordinary editing awkward. Giving a
Plugin a live filesystem path would instead let one running Generation observe
mutable state outside its Plan and would break Turn isolation during online
reconciliation.

The resource shape must stay part of the existing Plugin Instance model. A
second package type, mount DSL, global resource registry, or central App file
would recreate the overlapping concepts retired by ADR 0039.

## Decision

One configured Plugin Instance may have an adjacent resource directory with the
same name:

```text
plugins/<plugin-id>/
  <instance>.toml
  <instance>/
    prompts/system.md
    templates/report.md
```

The directory is optional and cannot exist without its matching Instance TOML.
It belongs only to that exact Instance, so two Instances of one Plugin may use
different configuration and different resources. A Session Profile selects the
Instance first; only resources for selected Instances enter its resolved App.

During Plugin Root resolution the Host recursively reads regular files into an
immutable, content-addressed snapshot. Paths are normalized relative UTF-8
paths. Symlinks and special files fail closed. Each Instance is limited to
4,096 files, 1 MiB per file, 16 MiB total, and 32 directory levels. Finder
`.DS_Store` files are inert metadata and are ignored at every level.

Resource digests and sizes enter the resolved Artifact Set and therefore the
App Generation identity. Resource edits wake the existing recursive Plugin
watcher and stage a complete candidate through the ordinary Ready Gate. A
failed scan or failed candidate leaves the current Generation routable. Existing
Turns keep the previous in-memory snapshot; Plugin code never receives the live
directory path.

The Runtime exposes an execution-class-neutral `InstanceResources` snapshot.
Native Rust Plugins receive it through `#[resources] InstanceResources` and may
read bytes or UTF-8 text by relative path. Other Execution Adapters must expose
the same immutable snapshot semantics through their language protocol before
claiming resource-directory support; they must not substitute a host path. The
Harness currently rejects a selected non-Native Instance that has a resource
directory instead of silently presenting an unreadable configuration.

Resources are non-secret App input. Credentials remain in environment or
provider-owned credential storage. Resource contents do not grant filesystem,
process, network, or Capability authority.

## Consequences

- ordinary Plugin configuration can use a small TOML file plus natural files;
- changing one resource creates a new Generation without changing the Plan;
- old and new Turns cannot observe each other's resource bytes;
- Profiles and multiple Instances need no resource overlay mechanism; and
- large or mutable datasets still require an explicit Plugin-owned Capability
  rather than this bounded configuration surface.

## Proof

Runtime tests prove deterministic content digests, path validation, immutable
reads, and Native factory injection. Harness tests prove resource directory
pairing, nested metadata handling, symlink rejection, retained old bytes, and a
new Generation identity after a resource-only edit.
