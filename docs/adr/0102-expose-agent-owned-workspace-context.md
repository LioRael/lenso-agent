# ADR-0102: Expose Agent-owned working directory context

Status: Accepted

## Decision

Agent Web may include `workspace: { path: string }` in bootstrap. The Web Host
captures its process working directory when its runtime starts. The field is
omitted if the directory cannot be represented as UTF-8. It is read-only
presentation context, not an editable path, project registry, Tool grant, or
assertion that all Tool roots equal that directory.

Console keeps Agent identity as the owner of every task, Session, and route.
It displays the directory supplied by that Agent, never its own server's cwd or
a browser-selected path. Older Agents remain usable without this optional field.
Choosing another connected Agent may select another working directory; changing
a running Agent's cwd or rebinding existing Sessions is outside this contract.
An existing Session may contain work from an earlier process directory, so the
UI describes this as the current Agent working directory, not historical binding.

The Changes view projects recorded git_diff/checkpoint_review Tool output. It
never treats a captured diff as a live repository query, reads arbitrary files,
or grants filesystem authority. Existing Tool providers remain the final owners
of path validation, execution, checkpoints, and mutations.

## Validation

The HTTP bootstrap test checks the actual child process directory. Console
validation covers missing/invalid metadata, Agent-qualified task links, and safe
rendering of recorded diff output.
