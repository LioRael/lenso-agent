# Harness parity delivery roadmap

This roadmap turns the mainstream Harness gap analysis into independently
deletable vertical slices. It is an execution order, not a claim that deferred
items already exist.

## Slice 1: Local coding workflow

Delivered by ADR-0056:

- official `code` and read-only `plan` Profiles;
- hierarchical `AGENTS.md` System Instruction contributions; and
- portable inline approval in interactive surfaces; and
- explicit Workspace checkpoints with bounded diff review, accept, and
  conflict-safe restore; and
- an explicit `code-sandbox` Profile backed by macOS Seatbelt or Linux
  bubblewrap, with read-only host files, Workspace-scoped writes, private
  temporary storage, and network denied by default; and
- typed Rust, JavaScript, Python, Go, and native-build program presets that
  resolve installed executable basenames into the model-visible Tool enum
  without exposing a shell or arbitrary executable path.

The local coding workflow slice is complete at this boundary. Stronger VM or
confidentiality isolation remains a separate security product, not hidden
scope inside this slice.

## Slice 2: Parallel coding supervision

Build on the existing bounded subagent task registry and durable child
Sessions. Items 1–5 and the end-to-end acceptance gate are delivered by
ADR-0059 through ADR-0066:

1. introduce named child Agent Instances selected by Profile rather than one
   shared child binding;
2. add a task supervisor projection with owner, status, Session, Generation,
   Workspace, and terminal result;
3. add a Worktree Provider Plugin that allocates one isolated checkout per
   mutation-capable child and never grants worktree authority to Kernel;
4. add background Process handles with bounded logs, cancellation, and durable
   terminal facts; and
5. render the same typed task snapshot in TUI and Web without making either
   surface the scheduler.

The parallel coding supervision slice is complete at this boundary. Its
end-to-end proof runs two mutation children in separate worktrees, keeps their
bounded progress visible to a reconnected Web client while the parent Workspace
remains unchanged, and integrates both exact reviewed commits only after an
explicit parent approval.

## Slice 3: Entrypoints and ecosystem

Expose the same Host and immutable Generation semantics through:

1. ACP stdio editor integration is delivered by ADR-0067; ADR-0068 targets the
   ACP Registry for Zed and records VS Code's missing public ACP registration
   surface instead of shipping an incompatible package;
2. a provider/model catalog with typed authentication and capability metadata;
3. Plugin discovery and marketplace UX over the existing Plugin Root and
   admission policy;
4. GitHub issue/PR/CI workflow Plugins;
5. browser and multimodal Tool Providers with explicit grants; and
6. OTLP traces plus a replay/evaluation runner grounded in Session facts.

Acceptance requires every entrypoint to resolve the same Profile to the same
Generation Spec digest, preserve Tool/approval policy, and emit comparable
Session provenance. No entrypoint may construct or mutate Kernel graphs.
