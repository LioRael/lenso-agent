# ADR 0034: Separate local user settings from the reviewed App Definition

## Status

Accepted.

Supersedes the persistence location selected by ADR-0031. The in-memory Plan
and Generation decisions from that ADR remain unchanged.

## Context

`lenso.app.json` is reviewed product source. It owns the base Module graph,
keyed Instances, bindings, placement, product defaults, and security policy.
Bundled Plugin enablement is instead a local user choice. Persisting that
choice under `extensions.lenso.agent.plugins` made an ordinary
`plugins enable` command dirty a Git checkout and conflated the product author
with every user of the product.

Moving the selection back into `.lenso/plugins` or an operating-system App
Data directory would recreate hidden private state. The user needs one
discoverable settings file containing only the choices they made.

Module configuration cannot be exposed as an unrestricted local patch.
Configuration includes network allowlists, workspace authority, resource
limits, and other product policy. A generic overlay could silently widen the
reviewed App's authority.

## Decision

The repository keeps the reviewed base App at `lenso.app.json`. A source-backed
Harness stores local user settings in `lenso.local.toml` beside that file:

```toml
schema_version = 1

[plugins]
enabled = ["skills@1", "text-tools@1"]
```

The root `.gitignore` excludes this exact file. It is created only after the
first non-default local choice and removed when no local choices remain.
`plugins status` always prints its path so the state is visible and editable.

The local schema is closed. It initially admits only the sorted, duplicate-free
bundled Plugin enabled set. Unknown sections, including arbitrary Module
overrides, fail closed. A future user-adjustable Module setting must be added as
a typed product setting with explicit validation and authority bounds; it must
not become a JSON merge patch over `app.modules[].configuration`.

For compatibility, a selection still present in the old
`extensions.lenso.agent.plugins` field is read when no local file exists. The
next successful enable or disable writes the local representation and removes
the legacy extension. New edits never add Plugin selection to
`lenso.app.json`.

On startup the Host combines the reviewed base App and typed local settings in
memory, expands exact Host-built Profiles, resolves a complete immutable Plan,
and stages the candidate Generation behind the Ready Gate. No resolved Plan,
Plugin Store, lock, Receipt, or Generation record is written.

Secrets remain outside both files.

## Consequences

- different users and checkouts can select different bundled Plugins without
  producing Git diffs;
- the product graph and security policy remain reviewable and reproducible;
- an empty local state produces no file;
- the Kernel still receives one closed immutable Plan; and
- Module-local preferences require an intentional typed product contract
  before they can be persisted locally.

## Proof

The source-backed integration test enables and runs `text-tools@1`, verifies
that `lenso.app.json` remains byte-identical, observes the exact
`lenso.local.toml`, disables the last selection, and verifies the local file is
removed. Additional tests prove a failed Ready Gate writes nothing, legacy
selection migrates on the next successful edit, arbitrary Module overrides are
rejected, and `.lenso/plugins` is never created.
