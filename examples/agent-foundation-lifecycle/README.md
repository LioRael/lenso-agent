# Agent foundation lifecycle consumer

This independent native Host composes an Agent through public Plugin contracts.
It is a synthetic two-step README review task, with no real model, network Tool,
coding tools, credentials, or external side effects. The official deterministic
Model drives the actual Loop, Tools runtime, and file-backed Session Plugin.
An external Tool Plugin owns the task checkpoint and its two local receipts.

The consumer lives outside the Agent workspace when executed. The verifier
copies this directory to a fresh location and points Cargo at explicit Core
and Agent source roots. These source patches select public package APIs; they
do not make private Rust items accessible. This is a source-local qualification,
not evidence that the packages have been published or that registry-only
installation succeeds.

## Run

Use the Agent checkout's pinned Rust toolchain and a Core checkout matching its
Cargo dependency revision. The verifier records both full source SHAs, tracked
diff hashes, fixture hash, tool versions, dependency paths, and Cargo logs.

```sh
python3 examples/agent-foundation-lifecycle/verify.py \
  --core-root /path/to/lenso \
  --agent-root /path/to/lenso-agent \
  --work-dir /tmp/agent-foundation-lifecycle-proof
```

The work directory must not exist and must be outside either source checkout.
Add `--offline` when the exact dependency closure is already cached. Builds
default to two Cargo jobs with a ten-minute command limit; each runtime phase
has a twenty-second process bound and five-second asynchronous bounds.

## Observable sequence

1. `start`: the official Model requests two Tool calls. The first fails before
   any effect and persists a retryable failure. The second explicitly retries
   and commits step one. A further turn reaches a pending second Tool step;
   cancellation propagates into that Tool and persists the cancelled checkpoint
   without committing step two. A separate pending Model turn is cancelled in
   the same Session.
2. `resume`: a new operating-system process reads the existing task checkpoint
   and Session through their owners. A new turn completes step two. A further
   read leaves the completed receipts unchanged. The process then exits with a
   Model turn open, without running shutdown or destructors.
3. `replay`: another process continues the same Session. The Loop records one
   `host_interrupted` terminal fact for the interrupted turn. Both cancelled
   turns remain terminal. More Tool calls leave the two completed receipts
   unchanged.

After replay, a fresh composition omits the task Plugin and its required
test-only caller. It starts and shuts down successfully, rejects the removed
caller's handle, and preserves the task's durable bytes.

The official Model regression is deliberate: after a cancelled
`Remain pending until cancelled.` request, the next user request must determine
the new completion behavior. Scanning all historical user requests makes the
resume phase wait forever; the fixture's timeout makes that a bounded failure.

The retained `evidence/` directory contains task checkpoints and resolved Plans
for each phase, canonical Generation documents, and Session events read through
the public Session capability. `lifecycle.log` contains process results.

## Ownership and support boundary

- The Host selects explicit Plugin Root instances and resolves them through
  `NativePluginRegistry::host_catalog` and `resolve_plugin_root`. It starts one
  immutable native generation per process, obtains canonical Generation
  provenance through the public Core resolver, and obtains model facts through
  the public Model catalog.
- The two removable example Plugins are the test caller and task Tool. The
  caller declares typed Agent, Session, Model, and Tool Provider requirements.
  The task Tool exposes the public Tool Provider capability and owns only its
  configured JSON checkpoint. It receives no workspace or process capability.
- File Session owns Session facts. The task Tool's checkpoint is associated
  task state, not a replacement Session history. Memory, Prompt, Compaction,
  and Artifact Plugins are explicit dependencies of the chosen default Loop.
- Retry is scripted input after a known pre-effect failure. Kernel does not
  decide retry policy. Restart starts a new turn using persisted facts; it
  neither revives a cancelled turn nor serializes a Rust Future.
- The task store is a trusted, single-writer fixture. Its effect is the local
  receipt committed with its state. Repeated reads of a completed task do not
  add receipts. A read of an unfinished task advances the next step: this is
  not an arbitrary operation-idempotency protocol, distributed transaction,
  general workflow engine, or proof that unknown external effects can be retried.
- Native code remains trusted. This does not qualify Wasm/Process isolation,
  authenticated multi-tenant Session access, package installation, automatic
  migrations, hot replacement, SDK publication, or a finished coding agent.

Blank Foundation intentionally provides composition diagnostics rather than
this execution policy. This example is an explicit execution Host, with all
selected behavior visible in its Plugin Root construction.
