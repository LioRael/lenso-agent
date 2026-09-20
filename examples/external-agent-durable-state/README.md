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

This is V05/V06 and native V09 source/local evidence. The verifier patches only its temporary
copy to sibling source paths. It does not qualify released artifacts or a clean
published install (V10).
