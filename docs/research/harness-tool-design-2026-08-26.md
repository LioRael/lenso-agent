# Agent Harness Tool 设计研究（2026-08-26）

## 结论先行

对 Codex、Claude Code、pi、DeepSeek Harness 和 OpenCode 的当前官方文档与源码做横向比较后，最值得 Lenso 采用的不是某一家完整的工具清单，而是下面这组共同收敛：

1. **模型面向的基础工具应当少、短、稳定，并尽量沿用模型已经熟悉的词。** 文件工具普遍收敛到 `read`、`glob`、`grep`、`edit`、`write`，命令工具收敛到 `bash`、`shell` 或 `exec_command`。五家里只有 Claude Code 系统性使用 PascalCase；其余新设计以小写单词或 snake_case 为主。
2. **Tool 名不是安全边界。** “模型能看见什么”、一次调用是否 `allow/ask/deny`、进程或文件系统实际能访问什么，应该是三层独立机制。Claude Code、Codex、DeepSeek Harness 和 OpenCode 都在不同程度上把 permission/approval 与 sandbox 分开。
3. **读、写、命令不能只有一条统一串行队列。** 先进实现已经把并发安全做成 Tool 或单次调用的属性：独立读取可以并行，写入要按目标串行或形成独占屏障，命令则根据语义与策略分类。
4. **结果契约必须同时服务模型、UI、持久化与调试。** 只有一段文本不够；至少需要模型可见内容、结构化值或元数据、稳定错误码、是否截断/可重试，以及调用身份。大输出应明确截断并提供可继续读取的 locator。
5. **Skill 应当是延迟加载的工作流说明，不应扩张成四个常驻模型工具。** Claude Code、DeepSeek Harness 和 OpenCode 都公开一个单数 `skill`/`Skill` 入口；pi 则只广告 Skill 元数据，再借已有 `read` 读取正文。Lenso 当前的 `skills.list`、`skills.read`、`skills.list_resources`、`skills.read_resource` 对运行时边界很清楚，但模型面过宽。
6. **Lenso 当前最有价值的基础不是 Tool 名，而是静态 App Composition、Provider 冲突 fail-closed、工作区根约束、create-only 写入、唯一精确编辑和结构化 Domain Error。** 应保留这些行为，再调整模型面命名和执行管线，不应为了模仿其它 harness 把工具注册表塞进 Kernel 或允许 live graph mutation。

本文把“官方事实”和“对 Lenso 的建议”分开书写。各项目均以 2026-08-26 可访问的官方文档或官方仓库为依据；源码链接尽量固定到本次研究时的 commit。

### 实施更新

本研究随后落地了第一阶段命名调整。内置 model-visible catalog 现在使用
`list`、`search`、`read`、`create_file`、`edit`、`run_process`、
`skill_list`、`skill`、`skill_resources`、`skill_resource` 和 `uppercase`。
内置 Tool 名不再包含 `text`，也不再使用点号；语义仍然保持 literal
search、create-only file creation 和 program-plus-arguments execution。ADR
0019 记录了这一决定，受影响 Module 通过 `0.2.0` package revision 形成新的
App Generation provenance。

## 研究范围与口径

- Codex：OpenAI 官方 `openai/codex` 仓库 main commit [`da4cf1c`](https://github.com/openai/codex/tree/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8)，辅以 OpenAI 官方模型工具指导。
- Claude Code：Anthropic 官方 [Tools reference](https://code.claude.com/docs/en/tools-reference)、[permissions](https://code.claude.com/docs/en/permissions)、[sandboxing](https://code.claude.com/docs/en/sandboxing)、[hooks](https://code.claude.com/docs/en/hooks)、[skills](https://code.claude.com/docs/en/skills) 和 [subagents](https://code.claude.com/docs/en/sub-agents)。Claude Code 客户端主体并非完整开放源码，因此不根据反编译或社区文章推断内部实现。
- pi：官方 `earendil-works/pi` 仓库 main commit [`8fa7eeb`](https://github.com/earendil-works/pi/tree/8fa7eebd235355522c8104166b4f1f959b4e2f10)。
- DeepSeek Harness：官方 `deepseek-ai/deepseek-harness` 仓库 master commit [`b150a55`](https://github.com/deepseek-ai/deepseek-harness/tree/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e)。
- OpenCode：官方 V2 文档。V2 明确仍是 beta，和 V1 并行存在，因此本文只把 V2 当作前沿方向，不把它描述为稳定契约；见 [V2 intro](https://opencode.ai/v2/docs)。

## 一、各 Harness 的已验证设计

### 1. OpenAI Codex

#### Tool catalog 与命名

Codex 的工具集合由当前模型、Feature、运行环境、MCP、动态工具和模式共同构建，不存在一个对所有会话都恒定的“完整默认清单”。官方源码的 Tool plan 条件注册了：

- 执行与长任务：`exec_command`、`write_stdin`；兼容路径还保留 `shell_command`。
- 文件与视觉：`apply_patch`、`view_image`。
- 控制工具：`update_plan`、`request_user_input`、`request_permissions` 等。
- MCP 资源：`list_mcp_resources`、`list_mcp_resource_templates`、`read_mcp_resource`。
- 多 Agent：新旧版本按 Feature 注册 `spawn_agent`、`send_message`、`followup_task`、`wait_agent`、`interrupt_agent`、`list_agents` 等。
- Code Mode：外层 `exec` 和 `wait`，再由代码调用嵌套工具。
- 延迟发现：`tool_search` 和可 defer-loading 的工具定义。

这些注册条件与 handler 可在固定版本的 [`spec_plan.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/spec_plan.rs) 中核对；例如 `update_plan` 的 JSON Schema 明确要求 `plan[]` 的 `step` 和 `status`，并限制 status 为 `pending | in_progress | completed`，见 [`plan_spec.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/handlers/plan_spec.rs)。

命名上，Codex 的公开模型工具主要使用小写 snake_case。源码的 `ToolName` 同时支持 namespace 与 name，但普通内置 function tool 落在默认 namespace；这意味着“内部可有命名空间”不等于“应把点号编码进模型看到的 name”。

#### Schema、结果与错误

Codex 同时支持 JSON function tool、freeform/custom tool、namespace tool 和 web/tool-search 等不同 ToolSpec。Router 将模型返回的 function/custom/tool-search call 统一解析为含 `tool_name`、`call_id`、payload 的 ToolCall，然后精确路由；解析失败或调用错误可以形成面向模型的错误，执行结果则转回 Responses API 的 input item。见 [`router.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/router.rs) 与 [`registry.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/registry.rs)。

`apply_patch` 是 freeform patch 语法而不是普通 JSON 编辑参数；OpenAI 的模型指导说明，命名的 freeform patch tool 相比无专用名称的做法降低了 patch 失败率。官方指导同时建议工具描述保持 1–2 句、服务器端验证 freeform 输入、独立读取并行、高影响写入后验证，见 [OpenAI model guidance](https://developers.openai.com/api/docs/guides/latest-model)。

#### 权限与 sandbox

Codex 把 approval policy 和 sandbox permissions 分开。Shell handler 可以先在默认 sandbox 下尝试，再对一次确切调用请求额外权限；在不允许请求升级的 approval policy 下，显式 escalation 会被拒绝而不是静默绕过。`apply_patch` 的 approval cache 还按多个目标分别记忆批准，使一次多文件 patch 只有在全部目标已批准时才跳过提示，见 [`shell.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/handlers/shell.rs) 与 [`sandboxing.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/sandboxing.rs)。

#### 并发与扩展

Tool runtime 逐工具暴露 `supports_parallel_tool_calls()`；Router 在调度前查询该属性，不能并行的工具不会和其它调用同走共享路径，见 [`registry.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/registry.rs) 和 [`router.rs`](https://github.com/openai/codex/blob/da4cf1cdeaf8fb44a18bb75fd8df0094097f90b8/codex-rs/core/src/tools/router.rs)。Codex 还支持 MCP、Skills、动态工具、deferred tool search，以及 Code Mode 的程序化工具调用；这里的核心启示是“注册/发现方式”和“执行权限”没有合并成一个布尔开关。

### 2. Anthropic Claude Code

#### Tool catalog 与命名

Claude Code 官方工具参考明确说表中的名称就是 permission rule、subagent tool list 和 hook matcher 使用的精确字符串。编码核心包括 `Read`、`Glob`、`Grep`、`Edit`、`Write`、`Bash`、`LSP`、`NotebookEdit`；交互与控制包括 `AskUserQuestion`、`Agent`、`EnterPlanMode`、`ExitPlanMode`、Task 系列、`Skill`、`ToolSearch`、MCP resource tools、Web tools 等，且部分工具只在满足 Feature、平台或产品条件时出现。完整且会持续变化的表见 [Tools reference](https://code.claude.com/docs/en/tools-reference)。

Claude 使用 PascalCase，这是五个对象里最明显的例外。其工具动词仍然非常朴素：`Read`、`Edit`、`Write`，没有把 workspace、filesystem、provider 名写进 Tool name。

#### 文件编辑和命令结果

`Edit` 是精确字符串替换，不是 regex 或 fuzzy edit；它要求 `old_string` 精确匹配并且默认唯一，可通过 `replace_all` 明确多处替换。Claude Code 还维护 read-before-edit 与文件变化检查，失败时促使模型重新读取。`Bash` 每次调用启动独立进程，提供超时、后台任务和大输出处理；成功大输出会保存到 session 文件并返回路径，失败大输出使用 head-and-tail 摘要。具体限制和状态规则见 [Tools reference 的 Edit/Bash 部分](https://code.claude.com/docs/en/tools-reference#edit-tool-behavior)。

#### 权限、sandbox 与 hooks

Permission rule 统一为 `Tool` 或 `Tool(specifier)`，例如 `Bash(git diff *)`、`Read(~/secrets/**)`、`Edit(/src/**)`、`Skill(deploy *)`、`Agent(Explore)`、`WebFetch(domain:example.com)`。Deny 在权限层阻止尝试；sandbox 则限制 Bash 即使被 prompt injection 诱导后实际能触达的文件和网络边界。文件 sandbox 还把 Read/Edit deny 规则合并进最终边界，见 [permissions](https://code.claude.com/docs/en/permissions)。

Hooks 覆盖 `PreToolUse`、`PermissionRequest`、`PostToolUse`、`PostToolUseFailure` 和一次并行批次结束后的 `PostToolBatch`。Pre hook 可以改写输入或阻止调用；post hook 可检查成功/失败。Hook 收到 `tool_name`、`tool_input` 等稳定字段，见 [hooks reference](https://code.claude.com/docs/en/hooks)。这证明权限判断、执行、结果观察是可插入的生命周期，而不是每个 Tool 自己弹对话框。

#### 并发、Skill 与 subagent

`PostToolBatch` 明确表示 Claude Code 存在并行工具批次。`Agent` 在独立上下文中运行 subagent；`tools` 与 `disallowedTools` 共同裁剪子 Agent 能用的工具，后者优先，且子 Agent 自己的调用仍经过用户 permission rules，见 [Agent tool behavior](https://code.claude.com/docs/en/tools-reference#agent-tool-behavior)。

Skill 使用单个 `Skill` Tool 执行工作流，Skill frontmatter 可用 `allowed-tools` 进一步约束。Skill 是“通过已有 Tool 执行的延迟工作流”，不是默认把每个 Skill 或 Skill resource 注册成一个新 Tool，见 [Skills](https://code.claude.com/docs/en/skills)。

### 3. pi coding agent

#### Tool catalog 与命名

pi 的 ToolName 当前精确定义为 `read | bash | powershell | edit | write | grep | find | ls`；源码导出每个工具的 definition factory 和可替换 operations。默认系统 prompt 在没有显式选择时使用 `read`、`bash`、`edit`、`write`，而 `grep`、`find`、`ls` 可按需启用。见官方 [`tools/index.ts`](https://github.com/earendil-works/pi/blob/8fa7eebd235355522c8104166b4f1f959b4e2f10/packages/coding-agent/src/core/tools/index.ts) 与 [`system-prompt.ts`](https://github.com/earendil-works/pi/blob/8fa7eebd235355522c8104166b4f1f959b4e2f10/packages/coding-agent/src/core/system-prompt.ts)。

pi 采用最短的小写名称，不做 namespace。它的设计哲学是用少量默认工具加一个非常开放的 Extension API，而不是在 core 中堆控制工具。

#### Schema、结果与扩展

Extension 通过 `pi.registerTool()` 注册 `name`、给用户看的 `label`、给模型的 `description`、TypeBox `parameters` 和 `execute`。结果为内容块 `content` 加 UI/session 使用的 `details`；执行期间可通过 `onUpdate` 流式更新。要让一次 Tool 失败并被标记为 `isError: true`，实现必须 throw，普通 return 无论包含什么字段都仍被视为成功。见官方 [Extensions 文档](https://github.com/earendil-works/pi/blob/8fa7eebd235355522c8104166b4f1f959b4e2f10/packages/coding-agent/docs/extensions.md)。

Extension 可以在 `tool_call` 前阻止调用，在 `tool_result` 后链式修改 `content`、`details` 或 `isError`；可以覆盖同名内置 Tool，也可以 `--no-tools` 后只加载扩展 Tool。其代价是扩展以宿主完整权限运行，官方明确要求只安装可信代码。同一文档还要求 Tool 截断输出，内置建议上限为 50KB 或 2000 行，并在截断时告知完整输出位置。

#### 权限与并发

pi core 没有 Claude/Codex 那样的统一强制 permission/sandbox 产品层；权限门禁可以由 Extension 的 `tool_call` hook 实现，bash 的 spawn hook 或可替换 operations 可接入容器、SSH 或自定义 sandbox。它是一种“最小、可嵌入、把政策交给宿主”的设计，不应被误写为已有安全沙箱。

文件变更有一个很值得借鉴的局部并发策略：write/edit 通过 file mutation queue 让同一路径的并发变更串行，而不是把所有读取和所有文件都塞入一条全局队列；实现入口可从 [`edit.ts`](https://github.com/earendil-works/pi/blob/8fa7eebd235355522c8104166b4f1f959b4e2f10/packages/coding-agent/src/core/tools/edit.ts) 和 [`file-mutation-queue.ts`](https://github.com/earendil-works/pi/blob/8fa7eebd235355522c8104166b4f1f959b4e2f10/packages/coding-agent/src/core/tools/file-mutation-queue.ts) 核对。

### 4. DeepSeek Harness

#### Tool catalog 与命名

DeepSeek Harness 是这批对象中 Tool Runtime 契约公开得最完整的一家。官方生成的 [Tool Schema Catalog](https://github.com/deepseek-ai/deepseek-harness/blob/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e/docs/tool-catalog.md) 会真实启动每个 shipped Tool plugin，再从 `ctx.tools.schemas()` 导出模型实际收到的 name、description 和 JSON Schema；CI completeness guard 防止新增 Tool 未被文档覆盖。

其常用工具包括：

- 文件与搜索：`read`、`read_image`、`write`、`edit`、`glob`、`grep`，以及另一套可选的 `str_replace_editor`。
- 命令与任务：`bash`/`pwsh`、`job_list`、`job_output`、`job_kill`，以及 opt-in terminal 系列。
- 交互与计划：`ask_user_question`、`exit_plan_mode`、`todo_write`。
- Skill、Web、LSP：`skill`、`web_fetch`、`web_search`、`lsp`。
- 子 Agent：`subagent`、`subagent_fork`、`send_message`、`interrupt_agent`、`list_agents`，另有实验性的 Agent Team 工具。
- 会话查询：`session_search`、`session_trace`、`session_event_read` 等。
- Code Mode：`run_code`；代码内的子调用重新进入完整的受保护 Tool pipeline。

全部使用小写 snake_case；包名和 model-visible name 分离，例如包 `@deepseek-ai/dsh-tool-fs` 对外只贡献 `read`、`write`、`edit`、`read_image`。

#### Registry、错误与持久化

Tool definition 有 schema、output contract、timeout、执行函数、纯 renderer、可选 metadata projector 和 `isConcurrencySafe(args)`。Registry 在注册时验证 schema 和语义；执行时把 JSON arguments 解析一次，走 `pre-execute → monotonic guards → execute wrappers → post-execute → finalizeContent → result`。Pre 可以 allow/deny/ask，guard 只能收紧不能放宽，post 可以检查或替换结果。见官方 [Tools subsystem](https://github.com/deepseek-ai/deepseek-harness/blob/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e/docs/subsystems/tools.md)。

结果把 execution-local canonical `value` 与持久化的 `content/error/meta` 分开；registry 会在最终 event 前把非 JSON、renderer 异常等统一物化成 JSON-safe error。调用身份留在不可变 ToolExecution 与 durable `tool/call`、`tool/result` 事件上，wrapper 不能伪造第二份身份。这个设计直接服务 replay：重放能恢复展示，但不假装重构进程内中间对象。

#### 权限、sandbox 与并发

ToolRestriction 是 scope 上的 allow/deny 可见性过滤；多个祖先 restriction 做交集，scope 自己注册的回答工具保持可用。它只决定“看得见什么”，不替代执行时 policy。

Sandbox policy 使用 `read-only | workspace-write | danger-full-access`，默认 `read-only`；一次调用解析出完整 mode + canonical workspace root。官方文档明确该 sandbox 当前只约束文件 effects，不声称限制网络和进程可见性。没有可用 sandbox backend 时返回 `SANDBOX_UNAVAILABLE` 并 fail closed；一次更宽模式的 retry 必须经过 approval，见 [`sandbox-policy/README.md`](https://github.com/deepseek-ai/deepseek-harness/blob/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e/packages/sandbox/sandbox-policy/README.md) 与 [Shell subsystem](https://github.com/deepseek-ai/deepseek-harness/blob/b150a551b8d465e31e418e1b2eaf5e79bbb7d28e/docs/subsystems/shell.md)。

并发不是 Tool 级死布尔值，而是 `isConcurrencySafe(args)` 对已验证参数分类。Agent loop 保持模型提交顺序：连续 `parallel` 调用进入有界 rolling pool；`exclusive` 调用先排空 pool、独占运行，并阻挡后续调用。Code Mode 子调用沿用同一规则并限制 `maxParallelSubCalls`。这是本文最推荐 Lenso 借鉴的调度模型。

### 5. OpenCode V2（beta）

#### Tool、权限与命名

OpenCode V2 的官方权限文档直接暴露当前动作/工具词汇：`read`、`edit`（同时管 `edit`、`write`、`patch`）、`glob`、`grep`、`shell`、`subagent`、`skill`、`question`、`webfetch`、`websearch`、`external_directory`，以及 Code Mode 的 `execute`。V2 特别提醒不要继续使用 V1 配置名 `permission`、`bash`、`task`，应使用 `permissions`、`shell`、`subagent`，见 [V2 permissions](https://opencode.ai/v2/docs/permissions)。

Permission 是有序规则数组，每条为 `{ action, resource, effect }`；一个操作涉及多个 resource 时，任一 `deny` 即拒绝，否则任一 `ask` 即询问，否则允许。`edit` action 故意覆盖 `edit`、`write`、`patch`，说明权限能力名不必和 Tool name 一一对应。外部目录另有 `external_directory` 边界。

#### Plugin、Skill、Code Mode

V2 Plugin 可以 transform agents/models/commands/integrations/references/skills/tools，也可以在模型请求前修改 tools record、在 Tool 执行前改输入、执行后改 result/output/outputPaths。自定义 Tool 以 JSON Schema 注册，结果可以同时返回 `structured` 与内容块；名字必须满足有限字符集，group 可形成前缀。默认 `codemode: true`，即通过 `execute` 暴露；设为 false 才直接暴露给 provider。见 [V2 Plugins](https://opencode.ai/v2/docs/build/plugins/)。

V2 Skill 每一步只广告 id/name/description；模型调用单个 `skill` 后才把正文加入 conversation，并给出最多十个 supporting paths 的样本，资源正文仍按需读取，见 [V2 Skills](https://v2.opencode.ai/docs/skills/)。这同时减少 prompt 体积和常驻 Tool 数量。

V2 仍是 beta，Plugin API 和命名可能变化，所以它最适合作为趋势证据，而不是 Lenso 现在就要兼容的外部标准。

## 二、横向比较

| 维度 | Codex | Claude Code | pi | DeepSeek Harness | OpenCode V2 |
|---|---|---|---|---|---|
| 核心文件词汇 | 主要靠 shell + `apply_patch` | `Read/Glob/Grep/Edit/Write` | `read/edit/write`，可选 `grep/find/ls` | `read/glob/grep/edit/write` | `read/glob/grep/edit/write/patch` |
| 命令词汇 | `exec_command` | `Bash`/`PowerShell` | `bash`/`powershell` | `bash`/`pwsh` | `shell` |
| 命名风格 | snake_case | PascalCase | lowercase | snake_case | lowercase/snake_case |
| 可见性裁剪 | Feature、模式、MCP、deferred loading | allowed/disallowed tool lists | active tool set | scope restriction | agent/request transform |
| 执行许可 | approval policy、per-call escalation | allow/ask/deny rules、hooks | 交给 extension/host | pre-execute + monotonic guards + approval | ordered action/resource/effect rules |
| Sandbox | OS/环境 sandbox，与 approval 分离 | Bash filesystem/network sandbox | 无内建统一安全边界 | file-effect sandbox，fail closed | external directory + permission；V2 文档未把它宣称为通用 OS sandbox |
| 并发 | per-tool support flag | parallel batch + PostToolBatch | 同路径 mutation queue | per-call safe/exclusive + bounded pool | Code Mode/Plugin 能力存在，公开文档未给出同等细的全局调度契约 |
| 结果 | call id + typed response item + hooks/telemetry | 成败分 hook，大输出 spill | content blocks + details + isError | value 与 durable content/error/meta 分离 | structured + content + output paths |
| Skill | Skill/插件/MCP | 单个 `Skill` | 元数据广告 + `read` | 单个 `skill` | 单个 `skill` 延迟注入 |
| 扩展代码信任 | MCP/动态工具按各自边界 | MCP/plugins/hooks | extension 全宿主权限 | trusted same-process registration，sandbox 在 capability seam | in-process plugin；V2 beta |

### 可观察的共同模式

1. **模型看到的 name 偏向语义，不偏向部署结构。** Provider/package/module 名留在运行时，模型看到 `read`，而不是 `filesystem-provider.read_text`。
2. **Schema 是产品界面。** description 不只解释参数，还告诉模型 fresh process、工作目录、超时、后台任务、截断和 policy denial 的恢复方式。
3. **同一个 Tool 可以在不同 scope 下可见性不同，但它的语义不变。** 这有利于 prompt cache、回放和跨模型稳定性。
4. **Mutation 要有并发前置条件。** 精确唯一替换、read-before-edit、content hash/version、同路径队列，至少要有一种；仅靠“模型一般不会同时写”不是契约。
5. **错误要让模型能采取正确下一步。** `not_found`、`invalid_arguments`、`permission_denied`、`content_changed`、`output_truncated`、`timed_out`、`cancelled`、`runtime_unavailable` 的恢复动作不同，不应都折叠成自由文本。

## 三、Lenso 研究时基线

本仓库当前有 11 个 model-visible Tool：

- `workspace.list`、`workspace.search`、`workspace.read_text`
- `workspace.write_text`、`workspace.edit_text`
- `process.exec`
- `skills.list`、`skills.read`、`skills.list_resources`、`skills.read_resource`
- 演示用 `text.uppercase`

定义可在 [`workspace-read`](../../crates/lenso-agent-workspace-read-module/src/lib.rs)、[`workspace-edit`](../../crates/lenso-agent-workspace-edit-module/src/lib.rs)、[`process-tools`](../../crates/lenso-agent-process-tools-module/src/lib.rs)、[`skills-filesystem`](../../crates/lenso-agent-skills-filesystem-module/src/lib.rs) 和 [`text-tools`](../../crates/lenso-agent-text-tools-module/src/lib.rs) 中核对。

研究开始时的 Tool Provider contract 为：

- catalog：`name`、`description`、字符串形式的 `input_schema_json`。
- execute：`name`、字符串形式的 `arguments_json`。
- success：单一 `text` content、`metadata_json`。
- domain failure：`invalid_arguments`、`permission_denied`、`not_found`、`output_limit_exceeded` 或带 `reason_code/message/details_json` 的 `execution_failed`。
- Tool Runtime 启动时聚合 Provider catalog，按名称排序，重复 Tool name 使 Resolved Plan 激活失败；运行时按精确 name 路由。

这些行为见 [`lenso.agent.tool-provider@1`](../../crates/lenso-capability-agent-tool-provider/src/contract.rs) 和 [`Tool Runtime`](../../crates/lenso-agent-tools-module/src/lib.rs)。

### 已经做对的部分

- App Composition 显式决定 Tool Provider，不存在内核级动态发现。
- Tool Provider 对资源政策和最终输出负责，聚合器只做 catalog、碰撞检查、校验和路由。
- workspace 路径拒绝绝对路径、`..` 和 symlink traversal；写入仅能创建新文件，编辑要求唯一精确匹配，并在落盘前检查内容未变化。
- Process Tool 不经 shell 解析，程序 allowlist、环境 allowlist、cwd、timeout 与 output 都有界。
- Provider Domain Error 与 Runtime Failure 分开，未知 Provider Error 仍保留 provider code。
- Skill 内容和资源在启动时 snapshot；resource Tool 明确“不执行脚本”。

### 当前差距

1. 两个 model adapter 都硬编码 `parallel_tool_calls: false`，Tool Capability admission 也为 `max_concurrency: 1`；模型无法提交读并行，runtime 也没有 safe/exclusive 分类。
2. `workspace.search` 实际是大小写敏感 literal search，却用了很泛的 `search`；它既不等同于模型熟悉的 `grep`，也没有 glob/filter/output mode。
3. `workspace.write_text` 的真实语义是 create-only；行业中的 `write` 通常意味着 create-or-overwrite。当前名称安全但冗长，若直接改成 `write` 会造成语义误导。
4. `process.exec` 不是 shell，不能接受 pipeline、redirect 或 shell syntax。若改名为 `shell`/`bash` 会错误暗示能力；但 `process.exec` 的点号也不是主流模型工具命名。
5. Skill 四 Tool 加上 prompt catalog 重复暴露 discovery；主流设计已经证明一个 lazy `skill` 足够承担模型入口。
6. success 只有 text，`metadata_json` 只是字符串；不能原生表示 image、structured value、spill locator、partial update 或 output schema。
7. Tool Definition 没有并发分类、side-effect/permission action、output schema、timeout/cancellation 能力或版本/来源身份。
8. permission 目前主要靠 Composition profile 与 Provider 内部 policy，没有统一 `allow/ask/deny` pipeline，也没有一次调用的批准与 sandbox escalation 协议。
9. `text.uppercase` 是好的 Plugin/替换验证 fixture，但不应出现在产品默认 Tool profile 或对外“已有工具”清单中。

## 四、建议的 Tool 设计

以下均为建议，不是当前实现事实。

### 4.1 先定义 Tool 的四层，而不是先加 Tool

建议明确四个彼此独立的层：

1. **Definition**：模型语义，包含稳定 name、description、input/output schema。
2. **Availability**：当前 App Composition 和 Agent scope 实际暴露的 definition 集合；限制只能收紧父 scope。
3. **Policy**：把已验证调用映射为 `allow | ask | deny`，按 action/resource 工作，不按 Tool 实现类工作。
4. **Execution Adapter**：真正执行文件、process、web、MCP；sandbox 在这里或其下方强制，绝不由 Tool name 暗示。

Tool Provider 仍是普通 Module；Tool Runtime 仍只依赖 resolved Provider bindings。新增 policy/approval/scheduler 应当是 Agent Harness 的 Capability/Module，而不是 `lenso-kernel` registry。

### 4.2 模型面命名规范

#### 两级命名模型

建议明确区分两个名字，避免继续让一个字符串同时承担持久身份、路由、权限配置和模型提示四种职责：

1. **稳定内部 ID（runtime/durable identity）**：全局唯一、带 major version，例如 `lenso.agent.workspace.read@1`。它进入 App Composition 校验、路由表、Session event、审计、回放、policy target 和 telemetry；Provider 被替换时语义 ID 不变，具体 `package_id/package_revision/provider_instance` 另存为 provenance。内部 ID 不受某个模型 provider 的 function-name 字符集限制，也默认不发给模型。
2. **短模型名（model-visible name）**：例如 `read`。它只服务本次模型请求中的选择和参数生成，必须短、熟悉、满足 transport 字符集。一个 request 内它必须与内部 ID 一一对应；adapter 不应悄悄把点号改成下划线后只保存别名。

一次调用应同时记录 `{ tool_id, exposed_name, provider_provenance, schema_version }`。模型返回 `exposed_name` 后，Runtime 只通过本次 immutable catalog snapshot 映射到 `tool_id`；不能在全局表里猜别名。这样可以在不破坏 durable Session 和 policy 的前提下，给不同模型 profile 选择 `read` 或极少数兼容别名，但同一 App Generation 内的映射保持字节稳定、无歧义。

内部 ID 的 major 表示语义或契约不兼容；短模型名不是版本号载体。权限优先绑定 `permission_action + canonical resource`，只有确需针对某个 Tool 时才绑定内部 ID，绝不绑定 adapter 临时别名。

建议定以下规则：

- 只用小写 ASCII snake_case：`^[a-z][a-z0-9_]{0,63}$`。
- 优先一个熟悉动词：`read`、`grep`、`edit`；需要区分时再加语义后缀，例如 `read_image`、`job_output`。
- 不在 name 中编码 Module、Provider、Capability 或 workspace：这些是 runtime provenance。
- 不用点号。内部仍可用 `(provider_id, tool_name)` 或 package identity 防冲突，但 model-visible name 保持可跨 provider 替换。
- 名称必须描述真实语义，不为追逐训练先验而说谎：argv execution 不能叫 `bash`，create-only 不能直接叫通用 `write`。
- 权限 action 与 Tool name 分离；`edit` action 可以同时管 `edit`、`write`、`apply_patch`。
- 内置保留名与第三方前缀规则由 Runtime 校验；冲突继续在 App Generation 激活前 fail closed，不做“最后注册者获胜”。

### 4.3 当前工具的建议改名

下面给出当前 11 个 Tool 的完整映射。内部 ID 是建议；“近期模型名”可用于一个明确的新 App variant，“目标模型面”表示语义升级或合并后的最终状态。

| 当前 model-visible name | 建议稳定内部 ID | 近期模型名 | 目标模型面 | 理由 |
|---|---|---|---|---|
| `workspace.read_text` | `lenso.agent.workspace.read@1` | `read` | `read` | 五家共同词汇；workspace root 是 policy，不是语义。后续 schema 加 offset/limit，但保留 UTF-8 事实 |
| `workspace.list` | `lenso.agent.workspace.list-directory@1` | `list` | pattern discovery 完成后由新 ID `lenso.agent.workspace.glob@1` 暴露 `glob` | 现在只是单目录 listing，不能冒充 glob；语义升级应新 ID，不偷换旧 ID |
| `workspace.search` | `lenso.agent.workspace.search@1` | `search` | regex/include/output-mode 完成后由新 ID `lenso.agent.workspace.grep@1` 暴露 `grep` | 当前只有 case-sensitive literal，`grep` 会过度承诺 |
| `workspace.edit_text` | `lenso.agent.workspace.edit@1` | `edit` | `edit` | 唯一精确替换正是 Claude/pi/DeepSeek 的成熟语义 |
| `workspace.write_text` | `lenso.agent.workspace.create-file@1` | `create_file` | 有 read/version precondition 的 create-or-overwrite 另建 `lenso.agent.workspace.write@1` 并暴露 `write` | 保留 create-only 安全语义；不能只改短名而扩大行为 |
| `process.exec` | `lenso.agent.process.run@1` | `run_process` | 真正提供 shell grammar 的 Adapter 另建 `lenso.agent.shell.execute@1` 并暴露 `shell` | 当前是 program + argv，无 shell 解析 |
| `skills.list` | `lenso.agent.skill.list@1` | `skill_list`（只在没有 prompt catalog 的兼容 profile） | 默认不暴露 | 当前 prompt catalog 已提供 discovery |
| `skills.read` | `lenso.agent.skill.load@1` | `skill` | `skill` | 单数 lazy loader 是 Claude/DeepSeek/OpenCode 的共同做法 |
| `skills.list_resources` | `lenso.agent.skill.list-resources@1` | `skill_resources`（兼容 profile） | 合入 `skill` 返回的 bounded manifest | 首次加载即可返回资源目录，减少常驻 Tool |
| `skills.read_resource` | `lenso.agent.skill.read-resource@1` | `skill_resource` | 若 authority 可安全映射则复用 `read`，否则保留 | Skill snapshot/version authority 可能和 workspace 不同，不能为了少一个 Tool 破坏边界 |
| `text.uppercase` | `lenso.example.uppercase@1` | `uppercase` | 产品 profile 不暴露 | 这是 Plugin/Composition 验证 fixture，不是 coding primitive |

一个重要取舍：**不要一次性只改名字而不做兼容策略。** Tool name 会进入模型 transcript 和 durable Session。建议 Capability major/version 或 App variant 明确切换，不在同一 catalog 长期同时暴露新旧别名；双别名会增加工具选择歧义，也可能让一次行为绕过按名字配置的 policy。

### 4.4 建议的最小 coding Tool profile

第一阶段建议只有：

```text
read
list
search
edit
create_file
run_process
skill
```

完成对应语义升级后，目标 profile 收敛为：

```text
read
glob
grep
edit
write
apply_patch        # 可选；有多文件原子/回滚契约后再加入
shell             # 可选；确实提供 shell grammar 的 Adapter 才加入
skill
ask_user          # 有 UI/非交互 provider seam 后加入
```

`lsp`、`read_image`、web、job、subagent、session query 都应是独立 App Composition 选择，不自动塞进 coding baseline。

### 4.5 Tool Definition v2 建议

建议把 JSON string 边界收紧为 source-first typed contract，再由 projection 生成 provider 所需 JSON：

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

其中 `execution` 不能只停留在 definition；对 `classify_per_call`，Provider 或单独 scheduler 在参数验证后返回 `parallel` 或 `exclusive(resource_keys)`。例如：

- `read/glob/grep/skill`：默认 parallel。
- `edit/write`：同一 canonical path 串行，不同 path 可并行；如果 App 希望更保守，可把全部 mutation 设为 exclusive。
- `apply_patch`：涉及所有 canonical target keys，作为一个原子调用；没有真正原子能力时要明确 partial result 和 recovery，不能假装原子。
- `run_process/shell`：默认 exclusive；未来只有显式证明无交互且无 mutation 的调用才标 safe，不能仅解析命令字符串后乐观放行。
- `ask_user`、plan transition、permission request：exclusive barrier。

### 4.6 Execute 与结果 v2 建议

建议 execution identity 由 Runtime 创建且不可被 Provider 改写：

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

持久化 `call_id/tool/validated arguments/result content/error/meta/provenance`；不要持久化进程内 canonical object 并声称可 replay。Runtime Failure 仍和 Tool Domain Error 分开：前者表示 Provider/Capability 不可用或协议破坏，后者表示一次合法调用的业务结果。

截断不是 error。结果要返回：采用 head 还是 tail、原始/保留字节数和行数、完整结果 locator；locator 必须继承原调用 authority，不能成为绕过 workspace/Skill 边界的裸路径。

### 4.7 统一执行管线

建议的 Harness 管线：

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

关键不变量：

- Ask 只能授权这次 canonicalized call 或一个明确可审计的 scope；不能把自然语言“同意”当作无限期 capability grant。
- Guard 只能收紧 pre policy，不能把 deny 改成 allow。
- Post hook 不能撤销已经发生的副作用；它只能拒绝/替换模型看到的结果并记录审计事实。
- Cancellation 从 Agent turn 传到 Tool、Process 和后台 job；超时后必须清理进程组。
- 同一 Tool call 只有一个 authoritative terminal outcome。

### 4.8 并发落地顺序

当前先不要把 adapter 的 `parallel_tool_calls` 直接改为 `true`。建议按以下顺序：

1. Session schema 先允许一批 model tool calls 有确定的 call order 和独立 terminal result。
2. Tool Definition/Provider 增加 concurrency classification。
3. Runtime 实现有界 rolling pool + exclusive barrier；并验证取消、timeout、一个调用失败时其它调用如何 settle。
4. 文件 mutation 加 canonical resource key 队列。
5. OpenAI adapters 才启用 `parallel_tool_calls`，fixture 覆盖乱序完成但顺序可重构。
6. 最后评估 `run_code`/`execute` Code Mode；其所有 nested call 必须重新进入同一 validation/policy/scheduler/result pipeline，绝不能成为捷径。

### 4.9 Skill 与扩展

建议把 Skill catalog 继续作为 Prompt Provider 的 bounded metadata contribution，只保留一个 model Tool：

```json
{
  "name": "skill",
  "arguments": { "name": "exact skill id" }
}
```

结果返回不可变 content version、正文和 bounded resource manifest。资源读取若继续走专用 Tool，必须以 `(skill id, content version, relative path)` 寻址，保证加载后不会漂移到另一个版本。

第三方 Tool Bundle 仍通过受审查的 Module package 和 App Composition 安装。原生 Rust、Bun 或安装包代码是 trusted code；sandbox 只约束其显式使用的 Adapter，不应宣称能限制任意 same-process Module。动态 Plugin 若未来存在，应通过 staged App Generation、readiness、switch、drain、rollback 改变下一代 catalog，而不是 mutation 当前 Tool Runtime。

### 4.10 Catalog 与契约验证

借鉴 DeepSeek Harness，建议增加一个生成的 Tool Schema Catalog：

- 从每个产品 Tool Provider 的真实 `catalog()` 结果生成。
- 包含 model-visible name、description、input/output schema、permission action、side effect、concurrency class、Provider package/version。
- CI 扫描所有发布的 Tool Provider package，任何遗漏失败。
- App variant snapshot 记录该 Composition 真正可见的 catalog，而不是只列“仓库实现过的 Tool”。
- 对各 model adapter 做 schema projection golden test，特别检查名称规范化；当前 direct Codex adapter 已存在点号到下划线映射，这类 transport alias 不应继续成为公开契约。

## 五、建议的决策

建议现在接受以下方向：

1. **公开命名规范采用 lowercase snake_case，不再新增带点号的 Tool name。**
2. **现有安全语义优先于短名。** 近期使用 `create_file`、`run_process`、`search`，等语义成熟后才升级为 `write`、`shell`、`grep`。
3. **Skill 模型面收敛为一个 `skill` Tool。** 原有四个 operation 可以留在内部 Capability 或下一 major 前的兼容 App 中，但不长期同时暴露。
4. **Tool Provider/Runtime v2 增加 typed structured results、stable error kind、permission action、side-effect 和 per-call concurrency classification。**
5. **先实现 guarded bounded parallel dispatch，再打开 provider 的 `parallel_tool_calls`。**
6. **权限采用 availability / allow-ask-deny policy / sandbox 三层，不按 Tool name 充当安全边界。**
7. **生成并验证每个 App Composition 的真实 Tool catalog。**
8. **Code Mode、动态 Tool loading 和 subagent Tool 后置。** 等直接 Tool pipeline 的权限、结果、回放、取消和并发都稳定后再加；否则只是把未解决的问题藏进一个更强的入口。

## 六、建议的实现分期

### Phase A：不改变执行语义的命名与可观测性

- 写 ADR：model-visible Tool naming + permission action vocabulary。
- 生成当前 App variants 的 Tool Schema Catalog。
- Tool result 增加明确 `truncated`/locator，保留现有文本兼容。
- 新建使用新名字的 App variant，不手改 resolved plan；对旧 Session/旧 composition 保留版本边界。

### Phase B：Tool contract v2

- source-first 定义 structured input/output/error。
- 增加 provenance、side-effect、permission action、cancellation、timeout。
- `skill` 单入口；`read` 支持 range；`search` 增加 include/exclude filter。
- 统一 pre/post hook 与 policy Capability，但保持 Provider 是 durable behavior owner。

### Phase C：安全并发

- rolling pool + exclusive barrier。
- canonical resource keys 与同路径 mutation queue。
- Agent loop/session 支持同一步多个 Tool call 与乱序完成。
- adapter 开启 parallel tool calls，并用 fixture、restart 和 cancellation 场景证明。

### Phase D：增强工具

- `glob`/`grep` 替换过渡工具。
- 带 version/hash precondition 的 `write`。
- 真正 shell Adapter 存在时加入 `shell`；继续保留 argv Process Capability 作为更窄的后端能力。
- 按 Composition 选择 `apply_patch`、`lsp`、`read_image`、jobs、web、subagent。
- 最后评估受 guard 的 `run_code`，并要求 nested calls 与 direct calls 拥有相同审计和回放证据。

## 最终判断

Lenso 不需要复制某一家 Harness 的表面清单。最佳组合是：

- 用 pi/DeepSeek/OpenCode 的短小 lowercase 工具词汇降低模型选择成本；
- 用 Claude Code 的精确 edit/read-before-write 和 permission rule 思路提高可预测性；
- 用 Codex 的 approval + sandbox 分层、freeform patch 和 per-tool并发经验；
- 用 DeepSeek Harness 的 typed pipeline、per-call concurrency、durable result 和生成 catalog；
- 继续坚持 Lenso 自己的 App Composition、Capability/Adapter seam、immutable Resolved App Plan 与 Generation 切换边界。

这样设计出来的 Tool 系统不是“工具更多”，而是每个工具名字更可信、每次调用更可审计、读任务能安全并发、写任务不会相互踩踏，并且未来替换 Provider 或增加 Plugin 时不需要改变 Kernel。
