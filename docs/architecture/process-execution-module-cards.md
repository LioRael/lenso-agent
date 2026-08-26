# Process execution Module cards

Status: implementation baseline for the higher-authority local-coding profile.

## `lenso.agent.process.native`

- **Deletion boundary:** removes the authorized program catalog and every local
  subprocess; workspace read, Prompt, Session, Agent Loop, Runner,
  Execution Adapter, and Kernel remain unchanged.
- **Owned facts:** canonical workspace root, allowed executable basenames and
  resolved identities, inherited environment allowlist, argument/time/output
  limits, cwd containment, child pipes, exit status, and process-group cleanup.
- **Provides:** private native `lenso.agent.process@1` (`catalog`, `run`).
- **Requires:** none.
- **Final authorization:** rejects unknown programs, invalid or escaping cwd,
  excessive arguments/time/output, executable identity changes, and unavailable
  roots. It clears the child environment before projecting configured values.
- **Lifecycle/resources:** each request owns one child process, isolated Unix
  process group, pipes, timeout, and drop guard. Completion disarms the guard;
  timeout, cancellation, output overflow, and dropped invocations kill the
  group and reap the direct child.
- **First behavior:** execute one structured argv without shell parsing and
  return bounded stdout, stderr, exit code, and duration.

## `lenso.agent.process-tools`

- **Deletion boundary:** removes `run_process` from the Tool catalog while the
  underlying Process Capability can remain usable by another explicitly bound
  consumer.
- **Owned facts:** Tool name, Model-facing JSON schema, default timeout, output
  presentation, metadata, and Process-to-Tool Domain Error mapping.
- **Provides:** `lenso.agent.tool-provider@2` (`catalog`, `execute`).
- **Requires:** exactly one private `lenso.agent.process@1` provider.
- **Final authorization:** none beyond argument decoding; it cannot expand the
  authoritative catalog returned by its bound Process Provider.
- **Lifecycle/resources:** activation obtains the generated Process client and
  snapshots its catalog; deactivation drops the client and Tool catalog.
- **First behavior:** project one provider-authorized process request into one
  `run_process` Tool call while forwarding the Kernel Invocation Context.

## Selection and trust

The two Modules are one atomic `local-process` Plugin selection. Disabling it
removes their Instances, bindings, package inputs, and now-unused private
contract, restoring the ordinary read-only graph.

This slice is not hostile-code isolation. Allowing Cargo, a test runner, or Git
can execute project code, hooks, aliases, or other configured behavior with the
host user's authority. There is no shell-string parser, but the profile must
still be selected only for reviewed code and workspaces. Command arguments and
outputs become durable Session facts and must not contain secrets.
