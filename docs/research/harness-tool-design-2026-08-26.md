# Agent Harness Tool Design Research (2026-08-26)

## Conclusions First

After comparing the current official documentation and source for Codex, Claude Code, pi, DeepSeek Harness, and OpenCode, Lenso should not adopt any one product's complete tool catalog. The most valuable result is the following shared convergence:

1. **Foundational model-facing tools should be few, short, stable, and use words the model already knows whenever possible.** File tools generally converge on `read`, `glob`, `grep`, `edit`, and `write`; command tools converge on `bash`, `shell`, or `exec_command`. Of the five, only Claude Code systematically uses PascalCase. The newer designs otherwise favor lowercase words or snake_case.
2. **A Tool name is not a security boundary.** “What the model can see,” whether a call is `allow/ask/deny`, and what a process or filesystem can actually access should be three independent mechanisms. Claude Code, Codex, DeepSeek Harness, and OpenCode all separate permission/approval from sandboxing to varying degrees.
3. **Reads, writes, and commands cannot share one universal serial queue.** Advanced implementations make concurrency safety a property of a Tool or individual call: independent reads can run concurrently, writes must serialize by target or form an exclusive barrier, and commands are classified by semantics and policy.
4. **The result contract must serve the model, UI, persistence, and debugging at the same time.** A single text string is insufficient. At minimum it needs model-visible content, a structured value or metadata, a stable error code, truncation/retry information, and call identity. Large output should be explicitly truncated and provide a locator from which reading can continue.
5. **A Skill should be a lazily loaded workflow description, not four permanently visible model tools.** Claude Code, DeepSeek Harness, and OpenCode expose a single `skill`/`Skill` entry point. pi advertises only Skill metadata, then reads the body through the existing `read` tool. Lenso's current `skills.list`, `skills.read`, `skills.list_resources`, and `skills.read_resource` establish clear runtime boundaries, but their model-facing surface is too broad.
6. **Lenso's most valuable current foundation is not its Tool names but static App Composition, fail-closed Provider conflicts, workspace-root constraints, create-only writes, unique exact edits, and structured Domain Errors.** Preserve those behaviors while adjusting model-facing names and the execution pipeline. Do not imitate other harnesses by moving a tool registry into the Kernel or permitting live graph mutation.

This document keeps “official facts” separate from “recommendations for Lenso.” Every project is based on official documentation or an official repository accessible on 2026-08-26; source links are pinned to the commit used for this research wherever possible.

### Implementation Update

The first naming phase was implemented after this research. The built-in model-visible catalog now uses `list`, `search`, `read`, `create_file`, `edit`, `run_process`, `skill_list`, `skill`, `skill_resources`, `skill_resource`, and `uppercase`. Built-in Tool names no longer contain `text` or periods; the semantics remain literal search, create-only file creation, and program-plus-arguments execution. ADR 0019 records this decision, and affected Modules use package revision `0.2.0` to establish new App Generation provenance.

## Research Scope and Standards

- Codex: the official OpenAI `openai/codex` repository at main commit [`da4cf1c`](https://github.com/openai/codex/tree/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8), supplemented by official OpenAI model/tool guidance.
- Claude Code: Anthropic's official [Tools reference](https://code.claude.com/docs/en/tools-reference), [permissions](https://code.claude.com/docs/en/permissions), [sandboxing](https://code.claude.com/docs/en/sandboxing), [hooks](https://code.claude.com/docs/en/hooks), [skills](https://code.claude.com/docs/en/skills), and [subagents](https://code.claude.com/docs/en/sub-agents). The main Claude Code client is not completely open source, so this document does not infer internals from reverse engineering or community articles.
- pi: the official `earendil-works/pi` repository at main commit [`8fa7eeb`](https://github.com/earendil-works/pi/tree/8fa7eebd235355522c8104166b4f1f959b4e2f10).
- DeepSeek Harness: the official `deepseek-ai/deepseek-harness` repository at master commit [`b150a55`](https://github.com/deepseek-ai/deepseek-harness/tree/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e).
- OpenCode: official V2 documentation. V2 explicitly remains beta and coexists with V1, so this document uses it only as evidence of an emerging direction, not as a stable contract; see the [V2 intro](https://opencode.ai/v2/docs).

## I. Verified Designs Across Harnesses

### 1. OpenAI Codex

#### Tool Catalog and Naming

Codex builds its tool set from the current model, Features, runtime environment, MCP, dynamic tools, and mode. There is no single “complete default catalog” that is constant for every session. The official source Tool plan conditionally registers:

- Execution and long-running work: `exec_command`, `write_stdin`; a compatibility path retains `shell_command`.
- Files and visual input: `apply_patch`, `view_image`.
- Control tools: `update_plan`, `request_user_input`, `request_permissions`, and others.
- MCP resources: `list_mcp_resources`, `list_mcp_resource_templates`, `read_mcp_resource`.
- Multi-Agent: old and new versions register `spawn_agent`, `send_message`, `followup_task`, `wait_agent`, `interrupt_agent`, `list_agents`, and others according to Feature state.
- Code Mode: outer `exec` and `wait`, with code invoking nested tools.
- Deferred discovery: `tool_search` and tool definitions that support deferred loading.

These registration conditions and handlers can be verified in pinned [`spec_plan.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/spec_plan.rs). For example, the JSON Schema for `update_plan` explicitly requires `step` and `status` in each `plan[]` item and restricts status to `pending | in_progress | completed`; see [`plan_spec.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/handlers/plan_spec.rs).

Public Codex model tools primarily use lowercase snake_case. The source `ToolName` supports both namespace and name, but ordinary built-in function tools use the default namespace. “Namespaces are available internally” therefore does not imply that periods should be encoded in the model-visible name.

#### Schemas, Results, and Errors

Codex supports JSON function tools, freeform/custom tools, namespace tools, web/tool-search, and other ToolSpec forms. The Router normalizes function/custom/tool-search calls returned by the model into a ToolCall containing `tool_name`, `call_id`, and payload, then routes them exactly. Parse failures or invocation errors can become model-facing errors, and execution results are converted back to Responses API input items. See [`router.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/router.rs) and [`registry.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/registry.rs).

`apply_patch` uses freeform patch syntax rather than ordinary JSON edit arguments. OpenAI's model guidance states that a named freeform patch tool reduces patch failures compared with an unnamed approach. The same official guidance recommends keeping tool descriptions to one or two sentences, validating freeform input server-side, parallelizing independent reads, and verifying high-impact writes afterward; see [OpenAI model guidance](https://developers.openai.com/api/docs/guides/latest-model).

#### Permissions and Sandbox

Codex separates approval policy from sandbox permissions. A shell handler can first attempt a call in the default sandbox and then request additional permission for that exact call. Under an approval policy that forbids escalation requests, explicit escalation is rejected rather than silently bypassed. The `apply_patch` approval cache also remembers approval separately for multiple targets, so a multi-file patch skips prompting only when every target has already been approved. See [`shell.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/handlers/shell.rs) and [`sandboxing.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/sandboxing.rs).

#### Concurrency and Extension

The Tool runtime exposes `supports_parallel_tool_calls()` per Tool. The Router queries it before scheduling; tools that do not support parallel execution do not use the shared parallel path with other calls. See [`registry.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/registry.rs) and [`router.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/router.rs). Codex also supports MCP, Skills, dynamic tools, deferred tool search, and programmatic Tool invocation through Code Mode. The key lesson is that “registration/discovery mechanism” and “execution permission” are not collapsed into one Boolean.

### 2. Anthropic Claude Code

#### Tool Catalog and Naming

Claude Code's official tool reference explicitly says that the listed names are the exact strings used in permission rules, subagent tool lists, and hook matchers. Core coding tools include `Read`, `Glob`, `Grep`, `Edit`, `Write`, `Bash`, `LSP`, and `NotebookEdit`; interaction and control include `AskUserQuestion`, `Agent`, `EnterPlanMode`, `ExitPlanMode`, the Task family, `Skill`, `ToolSearch`, MCP resource tools, Web tools, and others. Some appear only under particular Feature, platform, or product conditions. See the complete and evolving table in the [Tools reference](https://code.claude.com/docs/en/tools-reference).

Claude uses PascalCase, the clearest exception among these five systems. Its tool verbs are nevertheless plain—`Read`, `Edit`, and `Write`—and do not encode workspace, filesystem, or provider names into Tool names.

#### File Editing and Command Results

`Edit` is exact string replacement, not regex or fuzzy editing. It requires `old_string` to match exactly and, by default, uniquely; `replace_all` explicitly enables multiple replacements. Claude Code also enforces read-before-edit and file-change checks, prompting the model to reread after failure. Each `Bash` call starts an independent process and supports timeouts, background tasks, and large-output handling. Large successful output is saved to a session file and returns its path; large failed output uses a head-and-tail summary. See the concrete limits and state rules in the [Edit/Bash section of the Tools reference](https://code.claude.com/docs/en/tools-reference#edit-tool-behavior).

#### Permissions, Sandbox, and Hooks

A permission rule consistently takes the form `Tool` or `Tool(specifier)`, for example `Bash(git diff *)`, `Read(~/secrets/**)`, `Edit(/src/**)`, `Skill(deploy *)`, `Agent(Explore)`, or `WebFetch(domain:example.com)`. A deny rule prevents the attempt at the permission layer, while the sandbox limits which files and network boundaries Bash can actually reach even after prompt injection. The filesystem sandbox also merges Read/Edit deny rules into the final boundary; see [permissions](https://code.claude.com/docs/en/permissions).

Hooks cover `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PostToolUseFailure`, and `PostToolBatch` after one parallel batch. A pre-hook can rewrite input or block a call; a post-hook can inspect success or failure. Hooks receive stable fields such as `tool_name` and `tool_input`; see the [hooks reference](https://code.claude.com/docs/en/hooks). This establishes permission decisions, execution, and result observation as an interceptable lifecycle, rather than having every Tool display its own dialog.

#### Concurrency, Skill, and Subagent

`PostToolBatch` explicitly demonstrates that Claude Code has parallel Tool batches. `Agent` runs a subagent in an independent context. `tools` and `disallowedTools` jointly narrow what the child Agent can use, with the latter taking precedence, and the child Agent's own calls still pass through the user's permission rules; see [Agent tool behavior](https://code.claude.com/docs/en/tools-reference#agent-tool-behavior).

A Skill runs through one `Skill` Tool, and Skill frontmatter can narrow it further through `allowed-tools`. A Skill is “a lazily loaded workflow executed through existing Tools,” not a reason to register every Skill or Skill resource as a new Tool by default; see [Skills](https://code.claude.com/docs/en/skills).

### 3. pi Coding Agent

#### Tool Catalog and Naming

pi currently defines ToolName exactly as `read | bash | powershell | edit | write | grep | find | ls`. Source exports a definition factory and replaceable operations for each tool. Without an explicit selection, the default system prompt uses `read`, `bash`, `edit`, and `write`; `grep`, `find`, and `ls` can be enabled as needed. See official [`tools/index.ts`](https://github.com/earendil-works/pi/blob/8fa7eebd235355522c8104166b4f1f959b4e2f10/packages/coding-agent/src/core/tools/index.ts) and [`system-prompt.ts`](https://github.com/earendil-works/pi/blob/8fa7eebd235355522c8104166b4f1f959b4e2f10/packages/coding-agent/src/core/system-prompt.ts).

pi uses the shortest lowercase names without namespaces. Its philosophy is a small default tool set plus a very open Extension API, rather than accumulating control tools in core.

#### Schemas, Results, and Extensions

An Extension calls `pi.registerTool()` with `name`, a user-facing `label`, model-facing `description`, TypeBox `parameters`, and `execute`. Results contain `content` blocks plus `details` for UI/session use; `onUpdate` can stream updates during execution. To make a Tool fail and be marked `isError: true`, the implementation must throw. An ordinary return is still success regardless of its fields. See the official [Extensions documentation](https://github.com/earendil-works/pi/blob/8fa7eebd235355522c8104166b4f1f959b4e2f10/packages/coding-agent/docs/extensions.md).

An Extension can block before `tool_call` and chain modifications to `content`, `details`, or `isError` after `tool_result`; it can override a same-named built-in Tool or load only extension Tools after `--no-tools`. The tradeoff is that extensions run with full host permissions, and the official documentation explicitly says to install only trusted code. It also requires Tools to truncate output, recommends a built-in ceiling of 50 KB or 2,000 lines, and requires truncation to identify where the complete output can be found.

#### Permissions and Concurrency

pi core has no unified enforced permission/sandbox product layer comparable to Claude or Codex. An Extension's `tool_call` hook can implement a permission gate, while the bash spawn hook or replaceable operations can connect containers, SSH, or a custom sandbox. It is a “minimal, embeddable, host-owned policy” design and must not be misrepresented as an existing security sandbox.

File mutation has a valuable local concurrency policy: write/edit uses a file mutation queue to serialize concurrent changes to the same path, rather than putting all reads and all files into one global queue. Implementation entry points are [`edit.ts`](https://github.com/earendil-works/pi/blob/8fa7eebd235355522c8104166b4f1f959b4e2f10/packages/coding-agent/src/core/tools/edit.ts) and [`file-mutation-queue.ts`](https://github.com/earendil-works/pi/blob/8fa7eebd235355522c8104166b4f1f959b4e2f10/packages/coding-agent/src/core/tools/file-mutation-queue.ts).

### 4. DeepSeek Harness

#### Tool Catalog and Naming

DeepSeek Harness publishes the most complete Tool Runtime contract in this set. Its generated [Tool Schema Catalog](https://github.com/deepseek-ai/deepseek-harness/blob/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e/docs/tool-catalog.md) actually starts every shipped Tool plugin and exports the name, description, and JSON Schema the model receives from `ctx.tools.schemas()`. A CI completeness guard prevents a new Tool from going undocumented.

Common tools include:

- Files and search: `read`, `read_image`, `write`, `edit`, `glob`, `grep`, plus an alternative optional `str_replace_editor`.
- Commands and jobs: `bash`/`pwsh`, `job_list`, `job_output`, `job_kill`, and an opt-in terminal family.
- Interaction and planning: `ask_user_question`, `exit_plan_mode`, `todo_write`.
- Skill, Web, and LSP: `skill`, `web_fetch`, `web_search`, `lsp`.
- Subagents: `subagent`, `subagent_fork`, `send_message`, `interrupt_agent`, `list_agents`, plus experimental Agent Team tools.
- Session queries: `session_search`, `session_trace`, `session_event_read`, and others.
- Code Mode: `run_code`; nested calls from code reenter the complete protected Tool pipeline.

All use lowercase snake_case. Package names and model-visible names are separate; for example, package `@deepseek-ai/dsh-tool-fs` contributes only `read`, `write`, `edit`, and `read_image` externally.

#### Registry, Errors, and Persistence

A Tool definition has a schema, output contract, timeout, execution function, pure renderer, optional metadata projector, and `isConcurrencySafe(args)`. The Registry validates schema and semantics at registration. At execution it parses JSON arguments once and follows `pre-execute → monotonic guards → execute wrappers → post-execute → finalizeContent → result`. Pre can allow/deny/ask, guards can only tighten rather than relax, and post can inspect or replace results. See the official [Tools subsystem](https://github.com/deepseek-ai/deepseek-harness/blob/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e/docs/subsystems/tools.md).

Results separate execution-local canonical `value` from persisted `content/error/meta`. Before the final event, the registry materializes non-JSON values, renderer exceptions, and similar issues into JSON-safe errors. Call identity remains on immutable ToolExecution and durable `tool/call` and `tool/result` events; a wrapper cannot forge a second identity. This directly supports replay: replay can restore presentation without pretending to reconstruct in-process intermediate objects.

#### Permissions, Sandbox, and Concurrency

ToolRestriction is an allow/deny visibility filter on a scope. Restrictions from multiple ancestors intersect, while answer tools registered by the scope itself remain available. It determines only “what is visible” and does not replace execution-time policy.

Sandbox policy uses `read-only | workspace-write | danger-full-access`, defaulting to `read-only`; each call resolves a complete mode plus canonical workspace root. Official documentation states that the current sandbox constrains only filesystem effects and does not claim to restrict network or process visibility. If no sandbox backend is available, it returns `SANDBOX_UNAVAILABLE` and fails closed. A retry in a broader mode requires approval. See [`sandbox-policy/README.md`](https://github.com/deepseek-ai/deepseek-harness/blob/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e/packages/sandbox/sandbox-policy/README.md) and the [Shell subsystem](https://github.com/deepseek-ai/deepseek-harness/blob/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e/docs/subsystems/shell.md).

Concurrency is not a fixed Tool-level Boolean: `isConcurrencySafe(args)` classifies validated arguments. The Agent loop preserves model submission order: consecutive `parallel` calls enter a bounded rolling pool, while an `exclusive` call first drains the pool, runs alone, and blocks later calls. Code Mode nested calls use the same rules and a `maxParallelSubCalls` bound. This is the scheduling model this document most strongly recommends that Lenso adopt.

### 5. OpenCode V2 (beta)

#### Tools, Permissions, and Naming

OpenCode V2's official permissions documentation exposes its current action/tool vocabulary directly: `read`, `edit` (covering `edit`, `write`, and `patch`), `glob`, `grep`, `shell`, `subagent`, `skill`, `question`, `webfetch`, `websearch`, `external_directory`, and Code Mode `execute`. V2 specifically warns against retaining V1 configuration names `permission`, `bash`, and `task`; use `permissions`, `shell`, and `subagent`. See [V2 permissions](https://opencode.ai/v2/docs/permissions).

Permissions are an ordered rule array, each `{ action, resource, effect }`. When an operation touches multiple resources, any `deny` rejects it; otherwise any `ask` prompts; otherwise it is allowed. The `edit` action intentionally covers `edit`, `write`, and `patch`, showing that a permission capability name need not map one-to-one to a Tool name. External directories have a separate `external_directory` boundary.

#### Plugin, Skill, and Code Mode

A V2 Plugin can transform agents/models/commands/integrations/references/skills/tools. It can also modify the tools record before a model request, rewrite input before Tool execution, and modify result/output/outputPaths afterward. A custom Tool registers with JSON Schema and may return both `structured` data and content blocks. Names must satisfy a restricted character set, and a group can create a prefix. The default is `codemode: true`, which exposes it through `execute`; only false exposes it directly to the provider. See [V2 Plugins](https://opencode.ai/v2/docs/build/plugins/).

At each step, a V2 Skill advertises only id/name/description. Only after the model invokes the single `skill` Tool is the body inserted into the conversation, together with a sample of at most ten supporting paths; resource bodies remain on-demand. See [V2 Skills](https://v2.opencode.ai/docs/skills/). This reduces both prompt size and the permanently visible Tool count.

V2 remains beta and its Plugin API and naming may change. It is best used as trend evidence, not as an external standard Lenso should implement against now.

## II. Cross-System Comparison

| Dimension | Codex | Claude Code | pi | DeepSeek Harness | OpenCode V2 |
|---|---|---|---|---|---|
| Core file vocabulary | Primarily shell + `apply_patch` | `Read/Glob/Grep/Edit/Write` | `read/edit/write`, optional `grep/find/ls` | `read/glob/grep/edit/write` | `read/glob/grep/edit/write/patch` |
| Command vocabulary | `exec_command` | `Bash`/`PowerShell` | `bash`/`powershell` | `bash`/`pwsh` | `shell` |
| Naming style | snake_case | PascalCase | lowercase | snake_case | lowercase/snake_case |
| Visibility narrowing | Features, modes, MCP, deferred loading | allowed/disallowed tool lists | active tool set | scope restriction | agent/request transform |
| Execution authorization | approval policy, per-call escalation | allow/ask/deny rules, hooks | extension/host-owned | pre-execute + monotonic guards + approval | ordered action/resource/effect rules |
| Sandbox | OS/environment sandbox, separate from approval | Bash filesystem/network sandbox | no built-in unified security boundary | file-effect sandbox, fail closed | external directory + permission; V2 docs do not claim a general OS sandbox |
| Concurrency | per-tool support flag | parallel batch + PostToolBatch | same-path mutation queue | per-call safe/exclusive + bounded pool | Code Mode/Plugin capabilities exist; public docs do not specify an equally detailed global scheduler contract |
| Results | call id + typed response item + hooks/telemetry | separate success/failure hooks, large-output spill | content blocks + details + isError | value separate from durable content/error/meta | structured + content + output paths |
| Skill | Skills/plugins/MCP | single `Skill` | metadata advertisement + `read` | single `skill` | single `skill` with lazy injection |
| Extension-code trust | MCP/dynamic tools follow their respective boundaries | MCP/plugins/hooks | extension has full host permissions | trusted same-process registration, sandbox at capability seam | in-process plugin; V2 beta |

### Observable Shared Patterns

1. **Model-visible names favor semantics over deployment structure.** Provider/package/module names remain in the runtime; the model sees `read`, not `filesystem-provider.read_text`.
2. **Schema is a product interface.** A description explains not only parameters but also fresh-process behavior, working directory, timeout, background jobs, truncation, and recovery from policy denial.
3. **The same Tool may have different visibility in different scopes while retaining identical semantics.** This supports prompt caching, replay, and stability across models.
4. **Mutation requires a concurrency precondition.** At least one of unique exact replacement, read-before-edit, content hash/version, or a same-path queue is necessary. “The model generally will not write concurrently” is not a contract.
5. **Errors must let the model choose the correct next step.** `not_found`, `invalid_arguments`, `permission_denied`, `content_changed`, `output_truncated`, `timed_out`, `cancelled`, and `runtime_unavailable` require different recovery actions and should not all collapse into freeform text.

## III. Lenso Baseline at Research Time

The repository had 11 model-visible Tools:

- `workspace.list`, `workspace.search`, `workspace.read_text`
- `workspace.write_text`, `workspace.edit_text`
- `process.exec`
- `skills.list`, `skills.read`, `skills.list_resources`, `skills.read_resource`
- Demonstration Tool `text.uppercase`

Their definitions can be verified in [`workspace-read`](../../crates/lenso-agent-workspace-read-module/src/lib.rs), [`workspace-edit`](../../crates/lenso-agent-workspace-edit-module/src/lib.rs), [`process-tools`](../../crates/lenso-agent-process-tools-module/src/lib.rs), [`skills-filesystem`](../../crates/lenso-agent-skills-filesystem-module/src/lib.rs), and [`text-tools`](../../crates/lenso-agent-text-tools-module/src/lib.rs).

At the start of the research, the Tool Provider contract was:

- Catalog: `name`, `description`, and string-valued `input_schema_json`.
- Execute: `name` and string-valued `arguments_json`.
- Success: one `text` content value and `metadata_json`.
- Domain failure: `invalid_arguments`, `permission_denied`, `not_found`, `output_limit_exceeded`, or `execution_failed` with `reason_code/message/details_json`.
- At startup, Tool Runtime aggregates Provider catalogs, sorts by name, and makes Resolved Plan activation fail on duplicate Tool names. Runtime routing uses exact names.

These behaviors are defined by [`lenso.agent.tool-provider@1`](../../crates/lenso-capability-agent-tool-provider/src/contract.rs) and [`Tool Runtime`](../../crates/lenso-agent-tools-module/src/lib.rs).

### What Is Already Correct

- App Composition explicitly selects Tool Providers; there is no Kernel-level dynamic discovery.
- A Tool Provider owns resource policy and final output. The aggregator handles only catalog aggregation, collision checks, validation, and routing.
- Workspace paths reject absolute paths, `..`, and symlink traversal. Writes can only create new files. Edits require one unique exact match and verify that content has not changed before committing to disk.
- The Process Tool does not use shell parsing; its program allowlist, environment allowlist, cwd, timeout, and output are all bounded.
- Provider Domain Error is separate from Runtime Failure, and unknown Provider Errors retain the provider code.
- Skill content and resources are snapshotted at startup; the resource Tool explicitly “does not execute scripts.”

### Current Gaps

1. Both model adapters hard-code `parallel_tool_calls: false`, and Tool Capability admission is also `max_concurrency: 1`. The model cannot submit parallel reads, and runtime has no safe/exclusive classification.
2. `workspace.search` is actually case-sensitive literal search despite its broad `search` name. It is neither the model-familiar `grep` nor does it support glob/filter/output modes.
3. `workspace.write_text` is actually create-only, while industry `write` generally means create-or-overwrite. The current name is safe but verbose; renaming it directly to `write` would misrepresent its semantics.
4. `process.exec` is not a shell and cannot accept pipelines, redirects, or shell syntax. Renaming it to `shell`/`bash` would falsely imply capability, but the period in `process.exec` is also unlike mainstream model-tool naming.
5. Four Skill Tools plus a prompt catalog expose discovery twice. Mainstream designs demonstrate that one lazy `skill` entry point is enough for the model.
6. Success contains only text, while `metadata_json` is merely a string. It cannot natively represent an image, structured value, spill locator, partial update, or output schema.
7. Tool Definition lacks concurrency classification, side-effect/permission action, output schema, timeout/cancellation capability, and version/source identity.
8. Permission currently relies mainly on Composition profiles and Provider-internal policy. There is no unified `allow/ask/deny` pipeline or per-call approval and sandbox-escalation protocol.
9. `text.uppercase` is a useful Plugin/replacement verification fixture, but it should not appear in the product's default Tool profile or an external list of “available tools.”

## IV. Recommended Tool Design

Everything below is a recommendation, not a statement of current implementation.

### 4.1 Define Four Tool Layers Before Adding Tools

Define four independent layers explicitly:

1. **Definition**: model semantics, including stable name, description, and input/output schemas.
2. **Availability**: the actual definition set exposed by the current App Composition and Agent scope; restrictions may only narrow a parent scope.
3. **Policy**: maps a validated call to `allow | ask | deny`, operating on action/resource rather than Tool implementation class.
4. **Execution Adapter**: actually performs filesystem, process, web, or MCP work. Sandbox enforcement belongs here or below it and must never be implied by a Tool name.

A Tool Provider remains an ordinary Module, and Tool Runtime continues to depend only on resolved Provider bindings. New policy/approval/scheduler behavior should be an Agent Harness Capability/Module, not a `lenso-kernel` registry.

### 4.2 Model-Facing Naming Standard

#### Two-Level Naming Model

Distinguish two names explicitly instead of making one string carry durable identity, routing, permission configuration, and model prompting at once:

1. **Stable internal ID (runtime/durable identity)**: globally unique and major-versioned, for example `lenso.agent.workspace.read@1`. It participates in App Composition validation, routing tables, Session events, audit, replay, policy targets, and telemetry. When a Provider is replaced, the semantic ID stays constant while concrete `package_id/package_revision/provider_instance` is stored separately as provenance. The internal ID is not constrained by a model provider's function-name character set and is not sent to the model by default.
2. **Short model name (model-visible name)**: for example `read`. It serves only selection and argument generation in the current model request, so it must be short, familiar, and valid for the transport. Within one request it must map one-to-one to an internal ID. An adapter must not silently replace periods with underscores and retain only the alias.

Each call should record `{ tool_id, exposed_name, provider_provenance, schema_version }`. After the model returns `exposed_name`, Runtime maps it to `tool_id` only through the immutable catalog snapshot for that request; it must not guess aliases in a global table. Different model profiles can therefore choose `read` or a very small number of compatibility aliases without breaking durable Sessions or policy, while the mapping within one App Generation remains byte-stable and unambiguous.

The major version of an internal ID indicates semantic or contract incompatibility; a short model name is not a version carrier. Permissions should primarily bind `permission_action + canonical resource`, using the internal ID only when a Tool-specific rule is genuinely necessary, and never binding an adapter's temporary alias.

Recommended rules:

- Use only lowercase ASCII snake_case: `^[a-z][a-z0-9_]{0,63}$`.
- Prefer one familiar verb—`read`, `grep`, `edit`—adding a semantic suffix such as `read_image` or `job_output` only when distinction is necessary.
- Do not encode Module, Provider, Capability, or workspace into the name; these are runtime provenance.
- Do not use periods. Internal routing may still use `(provider_id, tool_name)` or package identity for collision prevention, while the model-visible name remains replaceable across Providers.
- Names must describe actual semantics, not lie for training priors: argv execution cannot be called `bash`, and create-only cannot be called general-purpose `write`.
- Separate permission actions from Tool names; one `edit` action may cover `edit`, `write`, and `apply_patch`.
- Runtime validates built-in reserved names and third-party prefix rules. Collisions continue to fail closed before App Generation activation; there is no “last registrant wins.”

### 4.3 Recommended Renames for Current Tools

The following is the complete mapping for the current 11 Tools. Internal IDs are recommendations. “Near-term model name” can be used in an explicit new App variant; “target model surface” describes the eventual state after semantic upgrades or consolidation.

| Current model-visible name | Recommended stable internal ID | Near-term model name | Target model surface | Rationale |
|---|---|---|---|---|
| `workspace.read_text` | `lenso.agent.workspace.read@1` | `read` | `read` | Shared vocabulary across all five systems; workspace root is policy, not semantics. Add offset/limit later while retaining the UTF-8 contract |
| `workspace.list` | `lenso.agent.workspace.list-directory@1` | `list` | after pattern discovery exists, expose `glob` from new ID `lenso.agent.workspace.glob@1` | Current behavior is one-directory listing, not glob; a semantic upgrade requires a new ID rather than silently changing the old one |
| `workspace.search` | `lenso.agent.workspace.search@1` | `search` | after regex/include/output-mode support exists, expose `grep` from new ID `lenso.agent.workspace.grep@1` | Current behavior is only case-sensitive literal matching; `grep` would overpromise |
| `workspace.edit_text` | `lenso.agent.workspace.edit@1` | `edit` | `edit` | Unique exact replacement is the mature Claude/pi/DeepSeek semantic |
| `workspace.write_text` | `lenso.agent.workspace.create-file@1` | `create_file` | add separate `lenso.agent.workspace.write@1` exposing `write` for create-or-overwrite with read/version preconditions | Preserve create-only safety semantics; a shorter name alone must not broaden behavior |
| `process.exec` | `lenso.agent.process.run@1` | `run_process` | add separate `lenso.agent.shell.execute@1` exposing `shell` only through an Adapter that truly provides shell grammar | Current behavior is program + argv, with no shell parsing |
| `skills.list` | `lenso.agent.skill.list@1` | `skill_list` (only in a compatibility profile without prompt catalog) | hidden by default | The prompt catalog already provides discovery |
| `skills.read` | `lenso.agent.skill.load@1` | `skill` | `skill` | A singular lazy loader is common to Claude/DeepSeek/OpenCode |
| `skills.list_resources` | `lenso.agent.skill.list-resources@1` | `skill_resources` (compatibility profile) | merge into the bounded manifest returned by `skill` | Initial loading can return the resource directory and reduce permanently visible Tools |
| `skills.read_resource` | `lenso.agent.skill.read-resource@1` | `skill_resource` | reuse `read` if authority can be mapped safely; otherwise retain | Skill snapshot/version authority may differ from workspace authority and must not be broken merely to remove a Tool |
| `text.uppercase` | `lenso.example.uppercase@1` | `uppercase` | hidden from product profiles | This is a Plugin/Composition verification fixture, not a coding primitive |

One important tradeoff: **do not rename everything at once without a compatibility strategy.** Tool names enter model transcripts and durable Sessions. Use an explicit Capability major/version or App variant transition, and do not expose old and new aliases together indefinitely in the same catalog. Dual aliases increase selection ambiguity and may allow behavior to bypass name-based policy.

### 4.4 Recommended Minimal Coding Tool Profile

The first phase should contain only:

```text
read
list
search
edit
create_file
run_process
skill
```

After the corresponding semantic upgrades, the target profile converges to:

```text
read
glob
grep
edit
write
apply_patch        # optional; add only after a multi-file atomicity/rollback contract exists
shell             # optional; add only when an Adapter truly provides shell grammar
skill
ask_user          # add after a UI/non-interactive provider seam exists
```

`lsp`, `read_image`, web, jobs, subagent, and session-query tools should be independent App Composition selections, not automatic additions to the coding baseline.

### 4.5 Recommended Tool Definition v2

Tighten the JSON-string boundary into a source-first typed contract, then generate the JSON projections required by providers:

```rust
struct ToolDefinition {
    name: ToolName,
    description: String,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
    permission_action: String,
    side_effect: SideEffect, // none | workspace_read | workspace_write | process | network | external_write
    execution: ToolExecutionClass, // parallel_safe | exclusive | classify_per_call
    timeout: ToolTimeoutPolicy,
    provenance: ToolProvenance, // package id/revision + provider instance, not model-visible by default
}
```

`execution` cannot remain only at definition level. For `classify_per_call`, the Provider or a separate scheduler returns `parallel` or `exclusive(resource_keys)` after argument validation. For example:

- `read/glob/grep/skill`: parallel by default.
- `edit/write`: serialize the same canonical path, while different paths may run concurrently; an App may conservatively make all mutations exclusive.
- `apply_patch`: use every canonical target key and treat the patch as one atomic call. Without true atomicity, return explicit partial-result and recovery information rather than pretending it was atomic.
- `run_process/shell`: exclusive by default. Only calls explicitly proven noninteractive and nonmutating should eventually be marked safe; never classify optimistically by merely parsing a command string.
- `ask_user`, plan transitions, and permission requests: exclusive barriers.

### 4.6 Recommended Execute and Result v2

Runtime should create execution identity, and a Provider must not be able to rewrite it:

```rust
struct ToolExecution {
    call_id: String,
    tool: ToolName,
    provider: ToolProvenance,
    arguments: JsonValue,
    parent_call_id: Option<String>,
    cancellation: CancellationToken,
}

struct ToolResult {
    content: Vec<ContentBlock>, // text | image | resource_link
    structured: Option<JsonValue>,
    metadata: JsonValue,
    truncated: Option<Truncation>,
}

struct ToolError {
    code: String,
    message: String,
    details: JsonValue,
    kind: ErrorKind, // invalid_input | denied | not_found | conflict | timeout | cancelled | unavailable | failed
    retryable: bool,
}
```

Persist `call_id/tool/validated arguments/result content/error/meta/provenance`. Do not persist an in-process canonical object and claim it can be replayed. Runtime Failure remains distinct from Tool Domain Error: the former means a Provider/Capability is unavailable or violated protocol; the latter is the business outcome of one valid call.

Truncation is not an error. A result should identify whether it kept the head or tail, original and retained byte/line counts, and a locator for the complete result. The locator must inherit the authority of the original call and must not become a bare path that bypasses workspace or Skill boundaries.

### 4.7 Unified Execution Pipeline

Recommended Harness pipeline:

```text
model call
  -> exact name lookup
  -> JSON/schema validation
  -> canonicalize resources
  -> availability restriction
  -> pre-execute policy (allow/ask/deny)
  -> monotonic guards
  -> concurrency classification
  -> bounded scheduler
  -> Provider execute through Capability/Adapter
  -> post-execute observation
  -> finalize/truncate/spill
  -> durable tool_result
  -> next model step
```

Key invariants:

- Ask may authorize only this canonicalized call or an explicit auditable scope; natural-language “approval” is not an indefinite capability grant.
- A guard may only tighten pre-policy and cannot convert deny to allow.
- A post-hook cannot undo a side effect that already happened; it can only reject or replace the result shown to the model and record an audit fact.
- Cancellation propagates from the Agent turn into the Tool, Process, and background job. A timeout must clean up the process group.
- One Tool call has exactly one authoritative terminal outcome.

### 4.8 Concurrency Implementation Order

Do not simply switch adapter `parallel_tool_calls` to `true` yet. Proceed in this order:

1. First allow one model step to contain a batch of model Tool calls with deterministic call order and independent terminal results in Session schema.
2. Add concurrency classification to Tool Definition/Provider.
3. Implement a bounded rolling pool plus exclusive barrier in Runtime, and verify cancellation, timeout, and how other calls settle when one call fails.
4. Add canonical-resource-key queues for filesystem mutations.
5. Only then enable `parallel_tool_calls` in OpenAI adapters, with fixtures covering out-of-order completion whose original order remains reconstructible.
6. Finally evaluate `run_code`/`execute` Code Mode. Every nested call must reenter the same validation/policy/scheduler/result pipeline and must never become a shortcut.

### 4.9 Skills and Extensions

Keep the Skill catalog as a Prompt Provider bounded-metadata contribution and expose only one model Tool:

```json
{
  "name": "skill",
  "arguments": { "name": "exact skill id" }
}
```

Its result returns immutable content version, body, and a bounded resource manifest. If resource reading continues through a dedicated Tool, address it by `(skill id, content version, relative path)` so a resource cannot drift to a different version after loading.

Third-party Tool Bundles remain installed through reviewed Module packages and App Composition. Native Rust, Bun, or installed package code is trusted code; a sandbox constrains only the Adapters it explicitly uses and must not be claimed to restrict an arbitrary same-process Module. If dynamic Plugins are added later, they should change the next-generation catalog through staged App Generation, readiness, switch, drain, and rollback—not mutate the current Tool Runtime.

### 4.10 Catalog and Contract Verification

Following DeepSeek Harness, add a generated Tool Schema Catalog:

- Generate it from each product Tool Provider's actual `catalog()` result.
- Include model-visible name, description, input/output schema, permission action, side effect, concurrency class, and Provider package/version.
- Have CI scan every published Tool Provider package and fail on any omission.
- Snapshot the catalog actually visible to each App variant Composition, rather than listing every Tool ever implemented in the repository.
- Add schema-projection golden tests for each model adapter, especially for name normalization. The current direct Codex adapter already maps periods to underscores; such transport aliases should not remain the public contract.

## V. Recommended Decisions

Accept the following direction now:

1. **Use lowercase snake_case for the public naming standard and add no new Tool names containing periods.**
2. **Existing safety semantics take priority over short names.** Use `create_file`, `run_process`, and `search` in the near term; upgrade to `write`, `shell`, and `grep` only after their semantics mature.
3. **Converge the Skill model surface on one `skill` Tool.** Existing four operations may remain in an internal Capability or a compatibility App until the next major version, but should not remain simultaneously exposed indefinitely.
4. **Add typed structured results, stable error kind, permission action, side effect, and per-call concurrency classification to Tool Provider/Runtime v2.**
5. **Implement guarded bounded parallel dispatch before enabling provider `parallel_tool_calls`.**
6. **Use three permission layers: availability, allow/ask/deny policy, and sandbox. Do not treat Tool names as a security boundary.**
7. **Generate and verify the actual Tool catalog of every App Composition.**
8. **Defer Code Mode, dynamic Tool loading, and subagent Tools.** Add them only after the direct Tool pipeline has stable permissions, results, replay, cancellation, and concurrency; otherwise they merely hide unresolved problems behind a more powerful entry point.

## VI. Recommended Implementation Phases

### Phase A: Naming and Observability Without Changing Execution Semantics

- Write an ADR for model-visible Tool naming and the permission-action vocabulary.
- Generate Tool Schema Catalogs for current App variants.
- Add explicit `truncated`/locator information to Tool results while retaining existing text compatibility.
- Create a new App variant using the new names; do not hand-edit the Resolved Plan. Preserve a version boundary for old Sessions and old compositions.

### Phase B: Tool Contract v2

- Define structured input/output/error source-first.
- Add provenance, side effect, permission action, cancellation, and timeout.
- Use one `skill` entry point; add range support to `read`; add include/exclude filters to `search`.
- Unify pre/post hooks and a policy Capability while keeping the Provider as the durable behavior owner.

### Phase C: Safe Concurrency

- Bounded rolling pool plus exclusive barrier.
- Canonical resource keys and a same-path mutation queue.
- Agent loop/Session support for multiple Tool calls in one step and out-of-order completion.
- Enable parallel tool calls in adapters and prove the behavior with fixture, restart, and cancellation scenarios.

### Phase D: Enhanced Tools

- Replace transitional tools with `glob`/`grep`.
- Add `write` with a version/hash precondition.
- Add `shell` only when a real shell Adapter exists; retain the argv Process Capability as a narrower backend capability.
- Select `apply_patch`, `lsp`, `read_image`, jobs, web, and subagent through Composition.
- Evaluate guarded `run_code` last, requiring nested and direct calls to have identical audit and replay evidence.

## Final Assessment

Lenso does not need to copy the surface catalog of any one Harness. The best combination is:

- Use the short lowercase tool vocabulary of pi, DeepSeek, and OpenCode to reduce model selection cost.
- Use Claude Code's exact edit/read-before-write and permission-rule ideas to improve predictability.
- Use Codex's approval/sandbox separation, freeform patch, and per-Tool concurrency experience.
- Use DeepSeek Harness's typed pipeline, per-call concurrency, durable results, and generated catalog.
- Continue enforcing Lenso's own App Composition, Capability/Adapter seam, immutable Resolved App Plan, and Generation-switch boundary.

The result is not a Tool system with “more tools,” but one in which every Tool name is more trustworthy, every call is more auditable, reads can run concurrently safely, writes do not trample one another, and future Provider replacement or Plugin addition does not require changing the Kernel.
