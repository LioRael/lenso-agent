# ADR 0037: Consume packaged Plugin Bundles without extraction

## Status

Accepted.

## Context

Directory discovery makes development simple but exposes filesystem copy
semantics to users. Even with bounded settling and an atomic staging convention,
the user must understand when a directory is complete. Conventional plugin
systems instead distribute one file that can be copied, inspected, removed,
and retained as one immutable unit.

Extracting an untrusted archive into the App tree would add path traversal,
symlink, partial cleanup, permission, and executable-file lifecycle concerns.
The existing admission path already consumes detached Manifest and Artifact
bytes and persists admitted Artifacts in the content-addressed Plugin Store, so
extraction is unnecessary.

## Decision

The Agent Harness accepts `.lenso-plugin` files as packaged Bundle inputs.

- The file is a ZIP container with `lenso-plugin.json` at its root and the same
  normalized relative files as a directory Bundle.
- The Host reads package entries directly into the existing bounded
  `LoadedBundle`; it does not extract them into the App or a temporary execution
  directory.
- Only Stored and Deflated entries are accepted. Encrypted entries, symlinks,
  non-file/non-directory entries, root escape, absolute or non-portable paths,
  duplicate paths, file/directory collisions, excessive depth, excessive entry
  count, excessive input bytes, and excessive expanded bytes fail closed.
- `enclosed_name` validation is required in addition to the stricter portable
  path rules. Archive names are never rewritten or sanitized into a different
  authority path.
- A packaged file and a directory Bundle enter the same Manifest parsing,
  Artifact verification, Profile matching, governance, quarantine, Ready Gate,
  Generation switch, rollback, and removal paths. The package format grants no
  authority.
- `plugins pack` reads an already bounded directory Bundle, canonical-parses its
  Manifest, writes deterministic ordered entries to a temporary file, syncs it,
  and atomically publishes or replaces the output.
- `plugins install` and `plugins upgrade` accept either representation.

Kernel, Module contracts, and App Generation authority formats remain
unchanged. Packaging is Harness authoring and Desired State input mechanics.

## Consequences

- The normal local workflow is one file copied into `plugins/`; removing that
  file stages the inverse Generation switch.
- A watcher can observe an incomplete package during copy, but quiet-period
  settling normally delays reading it. A malformed or persistently incomplete
  package is quarantined without changing the active Generation.
- Admission stores verified content by digest, so runtime execution does not
  depend on later archive extraction or mutable package contents.
- Other compression algorithms remain outside this slice.

## Rejected alternatives

Automatic extraction creates a second mutable Bundle tree and cleanup protocol.
Trusting the filename or ZIP metadata would bypass Manifest authority. Rewriting
unsafe paths changes what the publisher supplied and can collapse distinct
entries, so unsafe names are rejected instead.
