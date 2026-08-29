# ADR-0064: Compose bounded background process handles

Status: Accepted

## Context

The Process Capability already executes one authorized program with bounded
arguments, output, timeout, cancellation, and process-group cleanup. Its Tool
projection nevertheless waits for every process to finish. Long checks block a
Tool call, cannot be inspected through a stable handle, and cannot outlive the
Turn-scoped cancellation that launched them.

Moving process scheduling or handles into Kernel would violate the Host and
Plugin boundary. Duplicating background behavior in both native and sandbox
Providers would also create two observable contracts for the same Tool.

## Decision

`lenso.agent.process-tools` owns a Generation-local background process registry
over its existing bound `lenso.agent.process@1` stream. It exposes four Tools:

- `start_process` returns a UUID handle immediately;
- `read_process` returns one bounded snapshot and may release a terminal handle;
- `cancel_process` cancels only the selected process; and
- `list_processes` discovers every retained handle in stable order.

Each background task receives a fresh cancellation token and no Turn deadline,
while preserving the sealed Generation authority and Workspace scope required
by the selected Process Provider. Native and sandbox Providers therefore retain
final authorization, output limits, timeout, and process-group cleanup.

The Tool Plugin additionally requires the selected Session provider. A terminal
process appends one synthetic `tool_result` fact with schema
`lenso.agent.background-process@1`, its parent Session and Turn, bounded logs,
status, exit metadata, and cancellation reason. The existing Session contract
is unchanged. Agent Loop revision reconciliation advances only when every
conflicting fact is a validated background-process terminal; any other conflict
remains a concurrent-Turn rejection.

The registry has configured handle and combined-log limits. Logs truncate on a
UTF-8 boundary. Terminal handles remain visible until explicitly released, and
deactivation cancels every retained process without granting cleanup authority
to Kernel or either surface.

## Consequences

- long-running checks can continue after the starting Tool call returns;
- a reconnecting surface can rediscover handles through the same Generation;
- cancellation is process-scoped and bounded logs never grow without limit;
- terminal facts remain inspectable through the durable Session store; and
- Host restart recovery of live processes is not claimed by this decision.

## Proof

Unit coverage proves one combined UTF-8-safe log bound. Headless integration
runs a real background process through the official Profile, lists and reads
its handle, releases it after completion, and inspects the durable terminal
fact. A second real-process path cancels a long-running process and proves the
cancelled terminal fact is durable without cancelling the parent Turn.
