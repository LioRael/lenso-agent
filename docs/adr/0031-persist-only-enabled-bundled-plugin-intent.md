# ADR 0031: Persist only enabled bundled Plugin intent

## Status

Accepted.

Supersedes the bundled-Plugin persistence decisions in ADR-0022, ADR-0023,
and ADR-0026. Their Store-backed third-party lifecycle remains historical and
is outside this source-backed slice.

## Context

Users need to answer one product question: which optional capabilities are
enabled? The previous bundled workflow expanded that choice into
`.lenso/plugins/active-set.json`, Store objects, Receipts, Plugin locks,
Generation records, and Controller state. Moving those files to another hidden
directory would preserve the same unnecessary private database.

The immutable execution invariant remains valuable. The Kernel must still
receive one closed Plan, and a changed selection must still produce a fresh
candidate Generation that becomes Ready before it can replace the current
selection.

## Decision

`lenso.app.json` is the only persisted authority for bundled Plugin selection:

```json
{
  "extensions": {
    "lenso.agent.plugins": {
      "schema_version": 1,
      "enabled": ["skills@1", "workspace-edit@1"]
    }
  }
}
```

The enabled list is sorted and duplicate-free. A missing extension means no
optional bundled Plugins are enabled. Each `name@1` value selects one
versioned, Host-reviewed attachment Profile; it is not a package version.

The source document contains no Manifest, Receipt, Artifact path, permission
grant, binding, execution class, Plugin lock, resolved Plan, Generation digest,
timestamp, lifecycle record, or secret.

For every start or selection edit, the Host:

1. loads and validates the source App Definition and enabled IDs;
2. expands each ID through the exact bundled Profile Catalog and Host build;
3. synthesizes Manifest, Admission Receipt, Plugin lock, grants, and candidate
   Generation authority in memory;
4. resolves an immutable Plan using `NoArtifactSource`, which fails closed if a
   bundled Profile unexpectedly declares an Artifact;
5. stages the complete candidate behind the existing Ready Gate; and
6. compare-and-swaps `lenso.app.json` only after readiness succeeds.

Normal startup uses an in-memory Generation controller. It creates no
`.lenso/plugins` directory and writes no resolved Plan. Explicit `--plan`
remains an advanced exact-replay input but does not become the default
authoring workflow.

Third-party package acquisition, durable upgrade history, and durable rollback
are deliberately outside this slice. Legacy Store commands fail for a
source-backed App instead of silently recreating private Plugin state.

## Consequences

- `plugins available`, `status`, `enable`, and `disable` describe the same
  visible source authority.
- A failed resolution or Ready Gate leaves `lenso.app.json` byte-for-byte
  unchanged.
- Bundled Plugin startup and real execution require no Plugin Store.
- Kernel remains unaware of Plugins and continues to execute one immutable
  Plan per Generation.
- Process-crash recovery of in-memory bundled Generation state is replaced by
  deterministic reconstruction from `lenso.app.json`, Cargo locks, and the
  exact Host build.
- Durable third-party lifecycle design must use ordinary package acquisition
  and explicit App intent; it must not make a Lenso-private Store a prerequisite
  for bundled selection again.

## Proof

The source-backed integration test enables `text-tools@1`, verifies the exact
extension, runs a real Tool-backed Agent turn, disables the selection, and
asserts that `.lenso/plugins` never exists. A separate failure test supplies an
invalid Plan and verifies that the App Definition remains byte-identical.
