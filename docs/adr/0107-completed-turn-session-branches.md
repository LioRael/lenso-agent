# ADR 0107: Completed-turn Session branches and ordered activity

- Status: accepted
- Date: 2026-09-09

Console renders public execution events in their recorded order. Process text and Tool calls are distinct from the terminal answer. Completed Turns collapse the process by default; failures remain inspectable. Grouping and disclosure are presentation only and never rewrite Session history. Tool inputs, outputs, truncation and failures remain available in both Console chat surfaces. Missing historical media is not synthesized.

The Agent Web Host exposes POST sessions/{id}/fork with turnId and operationId. Only a completed Turn is an eligible boundary; the inclusive prefix is copied, preserving tool results, attachments and preceding compaction records. No model invocation, tool execution, filesystem rollback or worktree creation occurs. Branches share the current project directory. Active Turns retain their immutable Generation.

The Session Capability adds optional create_session_id to Open (version 1.8.0). It is mutually exclusive with session_id, whose open-existing meaning is unchanged. Both providers create or reopen the selected ID idempotently. The Host derives the branch ID from a validated UUID operation ID and atomically appends the prefix with expected revision zero. A process exit between creation and append leaves an empty retryable branch, never a partial prefix. Existing branches validate their durable fork_source before returning; operation IDs cannot silently select another source boundary.

SessionCreated records fork_source containing source session_id and turn_id. Session owns durable provenance and history; Artifact owns referenced bytes; the Host orchestrates the operation. Artifact references remain within the same Agent store. Copying a chat does not duplicate credentials, grant permissions, or transfer runtime resources. Console displays the source link and shared-directory semantics.
