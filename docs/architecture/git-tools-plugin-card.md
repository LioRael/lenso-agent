# Git Tools Plugin card

Status: implementation baseline for Profile-selected coding Agents.

## `lenso.agent.git-tools`

- **Deletion boundary:** removes `git_status`, `git_diff`, `git_log`,
  `git_stage`, and `git_commit`; Process, workspace Tools, Agent Loop, Session,
  Runtime, and Kernel remain unchanged.
- **Owned facts:** Tool names and schemas, semantic Git argv, read/mutation
  execution classes, literal path validation, history and commit-message bounds,
  output/error presentation, and Git-to-Tool error mapping.
- **Provides:** `lenso.agent.tool-provider@2` (`catalog`, `execute`).
- **Requires:** exactly one private `lenso.agent.process@1` provider whose
  catalog must authorize `git`.
- **Configuration:** default timeout, maximum log entries, and maximum commit
  message bytes. Workspace root and executable policy stay with Process.
- **Final authorization:** rejects unknown Tools, malformed arguments,
  absolute/parent/whole-repository stage paths, excessive path sets, excessive
  log entries, and empty/oversized commit messages. Process independently
  authorizes executable identity, cwd, argv bytes, timeout, output, and root.
- **Lifecycle/resources:** stateless. Each invocation owns only its Process
  Capability request; the bound provider owns child lifecycle and cleanup.
- **First behavior:** inspect a real repository, stage one explicit file,
  commit only its staged change without executing a failing repository hook,
  and read the resulting history.

## Profile selection

The Plugin is linked but absent from the default App. Configure Process and Git
Instances under `plugins/`, then select both exact Instances in a Session
Profile. Configuration remains beside each Plugin, not inside the Profile.

Removing only `lenso.agent.git-tools/default` restores the same Process-enabled
App without Git-specific Tools. Removing the Process provider while retaining
Git causes deterministic App resolution failure because the required Capability
is unsatisfied.
