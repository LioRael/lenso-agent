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
`plugins status --verbose` prints its path for diagnostics. The default status
keeps the local configuration file out of the ordinary Plugin workflow.

The local schema is closed. It initially admits only the sorted, duplicate-free
bundled Plugin enabled set. Unknown sections, including arbitrary Module
overrides, fail closed. A future user-adjustable Module setting must be added as
a typed product setting with explicit validation and authority bounds; it must
not become a JSON merge patch over `app.modules[].configuration`.

Discovered Bundle configuration follows the narrower adjacent-sidecar contract
in ADR-0038. That file is scoped to one Bundle stem and its selected Module
contributions; it is not a generic overlay in `lenso.local.toml`.

For compatibility, a selection still present in the old
`extensions.lenso.agent.plugins` field is read when no local file exists. The
next successful enable or disable writes the local representation and removes
the legacy extension. New edits never add Plugin selection to
`lenso.app.json`.

On startup the Host combines the reviewed base App and typed local settings in
memory, expands exact Host-built Profiles, resolves a complete immutable Plan,
and stages the candidate Generation behind the Ready Gate. No resolved Plan,
Plugin Store, lock, Receipt, or Generation record is written.

This no-Store rule applies to bundled selections, whose executable authority is
already closed by the exact Host build. A separately acquired third-party
Bundle retains its immutable Manifest, Receipt, Artifact, and Active Set in the
Plugin Store. The Host merges that third-party authority with the visible local
bundled selection in memory and Ready-checks the complete candidate before
committing install or removal. The Store is therefore artifact and admission
authority for acquired Releases, not a second database for bundled choices.

A visible `plugins/` directory beside the App Definition is a third source of
local Desired State. Each non-hidden immediate child is either one Bundle
directory or one packaged `.lenso-plugin` Bundle. The Host scans those entries
deterministically and admits only the narrow automatic policy shape:
a permission-free, stateless, isolated Wasm Tool that appends to the existing
Tool collection without Host imports. Discovery composes a transient Plugin
Set in memory and uses the Store only as a content-addressed artifact and
Receipt cache; it never writes discovered authority to `active-set.json`.
Long-lived Hosts use native filesystem events to wake Desired State
reconciliation and retain a low-frequency consistency scan for event loss or
unsupported filesystems. A changed valid subset is resolved and staged through
the existing overlap Generation Controller; the
routing epoch advances only after readiness, existing Turns keep their old
Leases, and directory removal stages the inverse switch. Broader, malformed,
or duplicate Bundles are quarantined and reported without making their
authority active or stopping the current Generation.

Long-lived surfaces persist their Controller lineage beneath the source App's
private `.lenso/plugins` authority root. The Host records the exact transient
source authority and Generation Spec before switching. Surface-specific
Controller directories prevent TUI, Telegram, Discord, and combined channels
from recovering one another's lineage. This derived recovery state does not
change `lenso.app.json` or `lenso.local.toml`; the one-turn headless CLI remains
process-local. Online source switches retain the exact predecessor under a
bounded rollback deadline. A Runner-terminal candidate failure restores only
that authorized predecessor; request-level failures leave Generation policy
unchanged.

Secrets remain outside both files.

## Consequences

- different users and checkouts can select different bundled Plugins without
  producing Git diffs;
- the product graph and security policy remain reviewable and reproducible;
- an empty local state produces no file;
- third-party Releases can be installed and removed without abandoning the
  source-backed App path;
- safe isolated Tool Bundles can be added and removed through a visible
  directory without a separate install transaction;
- a discovered Bundle can carry a separate local Module configuration sidecar
  without repacking the immutable Bundle;
- the Kernel still receives one closed immutable Plan; and
- Module-local preferences require an intentional typed product contract
  before they can be persisted locally.

## Proof

The source-backed integration test enables and runs `text-tools@1`, verifies
that `lenso.app.json` remains byte-identical, observes the exact
`lenso.local.toml`, disables the last selection, and verifies the local file is
removed. Additional tests prove a failed Ready Gate writes nothing, legacy
selection migrates on the next successful edit, arbitrary Module overrides are
rejected, and bundled-only selection never creates `.lenso/plugins`.
Source-backed third-party tests install a passive Release beside a bundled Tool
selection, execute an Artifact-backed QuickJS replacement after a fresh Host
start, remove acquired Releases through the checked transition path, and prove
a failed Ready Gate never commits the candidate Active Set. Discovery tests
build an independent Wasm Tool, run it after only copying its Bundle into
`plugins/`, prove directory removal withdraws its authority, and reject
governed, malformed, and duplicate Bundles. Online tests prove add/remove
switches routing through fresh Generations, an old Turn remains pinned during
the switch, and a blocked Bundle neither stops current routing nor prevents an
independent safe Bundle from becoming active. Durable tests prove graceful and
unclean recovery of the exact active source Generation and separate Controller
state for different surfaces. Automatic rollback tests prove the overlap edge
has a bounded window, a terminal candidate failure restores the exact previous
route, and durable status identifies the healthy rollback target and retired
failed Generation.
