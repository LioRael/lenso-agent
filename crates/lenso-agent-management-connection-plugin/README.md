# Delegated Management connection

This opt-in Native Plugin connects one Agent task to the selected application's
neutral Management catalog, invoke and status operations. It holds a newly
Auth-issued opaque child credential in a private file. The application verifies
the child and its current parent state on every request. Model arguments cannot
select a URL, deployment, subject, Session or credential.

The API and examples are source candidates. They have not been registry
published. Scoped delegation issuance is Native-only; Workers issuance is
explicitly unsupported until qualified.

The `management_task` example is an independently runnable embedded Agent Host.
Its catalog comes from source-generated Plugin Descriptors. The standard Core
Plugin Root loader and resolver derive its Plan. It links fixture Model, Loop,
durable Session, instructions and Management tools, with the Loop's required
bounded memory, compaction and artifact support. It selects no coding,
filesystem or arbitrary network Tool. This is distinct from a CLI-built source
App qualification.

```sh
cargo build -p lenso-agent-management-connection-plugin --example management_task
python3 crates/lenso-agent-management-connection-plugin/examples/prepare_task.py \
  --root /private/task-root --origin https://managed.example \
  --deployment deployment-a --credential-file /private/child-credential \
  --task-id task-1 --agent-session-id session-1
target/debug/examples/management_task --root /private/task-root \
  --prompt 'Read the managed state.' --receipt /private/read.json
```

The child file must be a regular, non-symlink file with owner-only permissions.
The preparer does not issue or print credentials. Configure the Auth owner with
the exact target caller `lenso.agent.management-connection/default`. Its signed
task and Agent Session labels must match the selected Plugin configuration.
When the same application serves Console's Cookie routes, select
`--delegated-route-prefix /agent` to use `/agent/management/catalog`,
`/agent/management/invoke` and `/agent/management/operations/<id>`. The Host
accepts only this fixed prefix or the empty default for `/management/*`.
It does not accept an arbitrary base path or a model-selected URL.

Deterministic prompts also include `Request an approved managed write.`,
`Query the managed operation.`, `Try a hidden management operation.` and
`Try to change the managed deployment.` The Host emits bounded events and shuts
down after each Turn. The same Plugin Root resumes its durable Session in a new
process. Pending approval remains a server-owned pending state. An uncertain
invoke is `unknown`; the connection never retries a write. Query an accepted
operation reference. Without one, reconcile the original intent identity with
the operator before another mutation.

```sh
python3 crates/lenso-agent-management-connection-plugin/examples/verify_task.py \
  --executable target/debug/examples/management_task --receipt /private/fixture.json
```

That verifier uses a transport fixture to test the actual Model/Loop/Session
graph, argument boundaries, unknown handling and credential privacy. It does
not claim Auth issuance or real parent revocation proof. The independent
application qualification must issue the child through Account's public scoped
delegation operation and verify current revocation against that owner.

Real model interaction acceptance and remote Hyperdrive acceptance are deferred
by the user. Neither is a dependency of the deterministic security vectors.
