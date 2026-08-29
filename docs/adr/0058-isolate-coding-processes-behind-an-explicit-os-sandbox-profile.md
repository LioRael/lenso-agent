# ADR-0058: Isolate coding processes behind an explicit OS sandbox Profile

Status: Accepted

## Context

The official `code` Profile constrains executable names, arguments, working
directories, environment, time, output, cancellation, and process-group
cleanup. Those policies do not prevent an allowed compiler, test, hook, or
project binary from writing outside the Workspace or opening a network
connection with the Host user's authority.

Approval policy cannot create that boundary. Moving the concern into Kernel or
the generic native Execution Adapter would also be incorrect: removing one
coding Process selection should remove this behavior, while other native
Plugins remain trusted in-process implementations.

## Decision

Add the replaceable linked Plugin `lenso.agent.process.sandbox`. It provides
the existing private `lenso.agent.process@1` Capability and retains final
authorization for its configured program catalog. It owns no durable state.
At Generation preparation it:

1. canonicalizes a Workspace root narrower than the filesystem root;
2. creates and protects a Host-configured temporary directory;
3. resolves and pins every allowed executable and the selected sandbox
   launcher;
4. rejects Host IPC endpoint variables from the environment allowlist; and
5. runs a real isolated `/usr/bin/true` readiness probe.

Each invocation gets a fresh temporary directory and executes through one
platform backend:

- macOS uses `/usr/bin/sandbox-exec` with a Seatbelt profile that allows host
  reads, Workspace and invocation-temporary writes, process descendants, and
  optionally network access; and
- Linux uses bubblewrap with a read-only root bind, writable Workspace, private
  `/tmp`, PID/session/IPC/UTS/cgroup/network namespaces, and `--share-net` only
  when network access is explicitly configured.

The provider preserves the Process Capability's structured
program-plus-arguments request, rooted relative cwd, output and argument
limits, live stdout/stderr stream, timeout, cancellation, and descendant
process-group termination. It revalidates executable identities before every
invocation. Unsupported platforms, missing launchers, unusable namespaces, or
a failed readiness probe reject the candidate Generation before routing.

`lenso-agent-cli profiles install coding` adds `code-sandbox`. The existing
`code` Profile deliberately continues to select
`lenso.agent.process.native`; changing an existing Profile's authority
silently would be a breaking policy change. `code-sandbox` selects exactly one
Process Provider and otherwise retains the coding, checkpoint, Git, approval,
Code Mode, and subagent composition.

## Security boundary

The official sandbox policy protects host filesystem integrity outside its two
writable roots and denies network egress. It is not a VM, a confidentiality
boundary, or a claim that allowed code is trustworthy: host files remain
readable, the Host kernel is shared, and platform sandboxes have distinct
attack surfaces. `allow_network = true` deliberately removes the egress
boundary while retaining filesystem isolation. Windows has no supported
backend in this release and fails readiness.

## Consequences

- deleting the sandbox Process Plugin and Profile removes the behavior without
  changing Kernel, Agent Loop, Process Tools, or Git Tools;
- native and sandboxed coding remain explicit separate authority choices;
- Git Tools inherit the selected Process Provider rather than spawning a
  second subprocess path;
- Linux Hosts must install bubblewrap and permit its namespace setup; and
- typed program presets remain bounded by the same Process Provider policy;
  stronger confidentiality/VM isolation remains an independent addition.

## Proof

Real macOS tests prove the backend readiness probe, Workspace write success,
external-file write denial, denial of a connection to a reachable Host loopback
listener, timeout, cancellation, descendant cleanup, and streamed output. Unit
and product tests prove configuration limits, exact program/cwd authorization,
official Profile installation, and successful resolution/readiness of
`code`, `code-sandbox`, and `plan`. Linux argument construction compiles with
the Linux implementation; a real Linux bubblewrap smoke remains a required CI
host gate before claiming that platform as empirically verified.
