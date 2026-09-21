# External Agent Durable State Fixture

This copied, source-local fixture proves that an independent Plugin can own
associated extension facts and durable task state without editing the Agent
Session event enum, default Agent Loop, or a Host-private module.

It uses public contracts only:

- `lenso.agent.extension-state@1` stores Plugin-owned, owner-derived facts;
- `lenso.agent.durable-task@1` stores a serializable approval state machine;
- the runner starts separate operating-system processes for start, recover,
  signal, and recovery-after-signal; and
- generic inspection hides the extension payload while reporting a stable
  digest and whether unknown state blocks recovery.

The Provider persists a small JSON fixture store. It is intentionally a test
implementation, not a general workflow product. Its effect state has no retry
operation: an `uncertain` workflow becomes observable-only on recovery.

Run the verifier from an Agent checkout:

```sh
python3 examples/external-agent-durable-state/verify-foundation.py \
  --framework-root /path/to/framework \
  --agent-root /path/to/lenso-agent
```

The removal phase keeps an active stream open, closes admission, proves that
retained request handles return `AdmissionClosed`, and observes the stream's
`Unavailable` terminal failure. Releasing that stream lease allows clean
shutdown. The replacement composition selects neither Plugin and cannot route
new calls. Removal preserves the durable store byte-for-byte.

The upgrade phase restarts the Provider, inspects the old task, and supplies a
recoverer that only understands state version 2. Version 1 remains readable;
recovery changes it to `UpgradeRequired` with one revision increment. Repeating
the check is idempotent, and a stale approval cannot resume the blocked task.
This tests state compatibility, not installation of a second binary artifact.

The child-policy phase cancels a parent and verifies that children and
grandchildren remain cancelled after restart. Deadlines are settled before
every public read or mutation, including restart, and expired tasks reject
late approvals. This fixture has no background timer while stopped. Removal
preserves the whole task tree; it cancels live transport work and leaves
durable tasks for explicit recovery when their owner is selected again.
Completed or uncertain-effect children are preserved for inspection, never
rewritten as if their external effects had been undone.

This is V05/V06 and native V09 source/local evidence. The verifier patches only its temporary
copy to sibling source paths. It does not qualify released artifacts or a clean
published install (V10).

For release preparation, package the two SDK contracts and consume their
versioned `.crate` archives from an independent directory:

```sh
cargo package -p lenso-capability-agent-durable-task -p lenso-capability-agent-extension-state
python3 examples/external-agent-durable-state/verify-artifacts.py --artifacts-dir target/package
```

This verifier unpacks the archives, replaces only the external example's direct
SDK dependencies with those unpacked packages, and resolves all other Lenso
dependencies from crates.io. It uses no Cargo patches or sibling source paths.
It prints archive digests and the consumer lock digest. This proves prepared
artifact consumption; it does not publish a package or establish registry
availability. The SDKs remain excluded from automatic publication.

## Upgrade between real binaries

Run `python3 examples/external-agent-durable-state/verify-binary-upgrade.py`.
The verifier builds three distinct native Provider executables in a clean
registry-only consumer and checks their artifact versions and SHA-256 hashes.
A compatible v2 Provider recovers the v1 store without changing its bytes,
including uncertain external effects. An incompatible v3 Provider refuses v1
state even if the caller claims it supports v1; retained payloads stay readable
and old approval cannot resume the task. These are fixture releases, not
published SDK versions. No migration or package-manager hot replacement is
implied. The file store remains a single-writer test Provider.
