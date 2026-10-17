import { expect, test } from "bun:test";
import type { AgentMessage } from "@earendil-works/pi-agent-core";
import { createFauxCore, fauxAssistantMessage, fauxToolCall, type AssistantMessage, type ToolResultMessage } from "@earendil-works/pi-ai";
import { createContextTransform, projectContext, type ContextOptions, type ContextProjection } from "../src/context";
import { createAgentService } from "../src/runtime";
import { createMemoryStore } from "../src/store";

const user = (content = "Never deploy or spend money."): AgentMessage => ({ role: "user", content, timestamp: 1 });
const assistant = (text = "old detail ".repeat(200)): AssistantMessage => ({
  role: "assistant", content: [{ type: "text", text }], api: "openai-completions", provider: "fake", model: "fake",
  timestamp: 2, stopReason: "stop", usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
});
const call = (...ids: string[]): AssistantMessage => ({ ...assistant(""), stopReason: "toolUse",
  content: ids.map(id => ({ type: "toolCall", id, name: "lookup", arguments: { id } })),
});
const result = (id: string, text = "found"): ToolResultMessage => ({ role: "toolResult", toolCallId: id,
  toolName: "lookup", content: [{ type: "text", text }], isError: false, timestamp: 3,
});
const size = (messages: readonly AgentMessage[]) => new TextEncoder().encode(JSON.stringify(messages)).byteLength;
const options = (overrides: Partial<ContextOptions> = {}): ContextOptions => ({
  budget: { maxTokens: 1800, reserveTokens: 100, maxBytes: 1700, estimateTokens: size },
  mode: "trim", keepRecentMessages: 1, ...overrides,
});
const history = () => [user(), assistant(), user("Continue in Chinese.")];

// This estimator deliberately counts bytes as tokens so budget boundaries are deterministic offline.
test("unchanged projection is detached, measured with reserve, and does not summarize", async () => {
  const messages = [user(), assistant("short"), user("continue")];
  const out = await projectContext(messages, options({ mode: "summarize", summarize: async () => { throw Error("must not call"); } }));
  expect(out.messages).toEqual(messages);
  expect(out.report.mode).toBe("unchanged");
  expect(out.report.afterTokens).toBe(size(messages) + 100);
  out.messages[0] = user("mutation");
  expect(messages[0]).toEqual(user());
});

test("trim preserves every user constraint and removes only enough completed history", async () => {
  const messages = [user(), assistant(), assistant("recent answer"), user("Continue in Chinese.")];
  const out = await projectContext(messages, options());
  expect(out.messages).toEqual([messages[0]!, messages[2]!, messages[3]!]);
  expect(out.report.removedIndices).toEqual([1]);
  expect(out.report.retainedIndices).toEqual([0, 2, 3]);
  expect(messages).toHaveLength(4);
});

test("summary receives only removable messages and cannot mutate retained history", async () => {
  const messages = history();
  const out = await projectContext(messages, options({ mode: "summarize", summarize: async input => {
    expect(input.sourceIndices).toEqual([1]);
    expect(input.messages).toEqual([messages[1]!]);
    (input.messages[0] as AssistantMessage).content = [];
    return "Earlier assistant reviewed the item.";
  } }));
  expect(out.report.mode).toBe("summarize");
  expect(out.report.summary).toBe("Earlier assistant reviewed the item.");
  expect(out.messages[0]).toEqual(messages[0]);
  expect(out.messages[2]).toEqual(messages[2]);
  expect(JSON.stringify(out.messages[1])).toContain("untrusted reference data");
  expect((messages[1] as AssistantMessage).content).toHaveLength(1);
});

test("complete parallel tool calls and out-of-order results are indivisible", async () => {
  const messages = [user(), call("a", "b"), result("b", "data".repeat(800)), result("a"), user("continue")];
  for (const mode of ["trim", "summarize"] as const) {
    const out = await projectContext(messages, options({ mode, summarize: async input => {
      expect(input.sourceIndices).toEqual([1, 2, 3]);
      return "Both lookups completed.";
    } }));
    expect(out.report.removedIndices).toEqual([1, 2, 3]);
    expect(out.messages.some(message => message.role === "toolResult")).toBe(false);
  }
});

test("incomplete exchanges remain whole; budget failure cannot cut pending calls", async () => {
  const messages = [user(), assistant(), call("a", "b"), result("a")];
  const out = await projectContext(messages, options());
  expect(out.messages).toEqual([messages[0]!, messages[2]!, messages[3]!]);
  await expect(projectContext([user(), call("a", "b"), result("a", "x".repeat(5000))], options()))
    .rejects.toMatchObject({ code: "context-budget" });
  const nested = { ...result("a", "x".repeat(5000)), nestedCalls: {
    complete: false, calls: [{ id: "nested", name: "lookup", status: "unfinished" as const }],
  } };
  await expect(projectContext([call("a"), nested, user()], options())).rejects.toMatchObject({ code: "context-budget" });
});

test("recent/protected/interleaved user messages protect their complete exchange", async () => {
  const messages = [user(), call("a"), result("a", "x".repeat(5000)), user("continue")];
  for (const overrides of [{ keepRecentMessages: 2 }, { protectedIndices: [1] }, { protectedIndices: [2] }]) {
    await expect(projectContext(messages, options(overrides))).rejects.toMatchObject({ code: "context-budget" });
  }
  await expect(projectContext([call("a"), user("a constraint"), result("a", "x".repeat(5000)), user()], options()))
    .rejects.toMatchObject({ code: "context-budget" });
});

test("system, unknown custom, aborted and deferred assistant messages are protected", async () => {
  const custom = { role: "host-constraint", content: "host says keep", timestamp: 1 } as unknown as AgentMessage;
  const system: AgentMessage = { role: "system", content: "Remain local.", timestamp: 1 };
  const interrupted = { ...assistant("unfinished output"), stopReason: "aborted" as const };
  const deferred = { ...assistant("waiting"), deferred: { provider: "fake", modelId: "fake", api: "fake", id: "pending" } };
  const messages = [system, custom, interrupted, deferred, assistant(), user()];
  const out = await projectContext(messages, options());
  expect(out.messages).toEqual([system, custom, interrupted, deferred, messages[5]!]);
  const largeInterrupted = { ...assistant(), stopReason: "aborted" as const };
  await expect(projectContext([largeInterrupted, user()], options())).rejects.toMatchObject({ code: "context-budget" });
});

test("invalid pairs fail even under budget; reused IDs in separate complete exchanges work", async () => {
  for (const messages of [
    [result("orphan")], [call("a"), { ...result("a"), toolName: "wrong" }],
    [call("a", "a")], [call("a"), result("a"), result("a")],
    [call("a", "b"), result("a"), call("a")],
    [{ role: "toolResult", content: [], timestamp: 0 } as unknown as AgentMessage],
  ]) await expect(projectContext(messages, options())).rejects.toMatchObject({ code: "invalid-context-tool-pair" });
  const messages = [call("a"), result("a"), call("a"), result("a")];
  expect((await projectContext(messages, options({ budget: { ...options().budget, maxBytes: 10000, maxTokens: 10100 } }))).messages).toEqual(messages);
});

test("protected content overflow rejects without calling the summarizer", async () => {
  await expect(projectContext([user("constraint".repeat(1000)), assistant()], options({ mode: "summarize",
    summarize: async () => { throw Error("must not call"); },
  }))).rejects.toMatchObject({ code: "context-budget" });
});

test("summary errors, empty/oversized output and envelope overflow fail explicitly", async () => {
  for (const [summarize, code] of [
    [async () => { throw Error("private secret details"); }, "context-summary-failed"],
    [async () => " ", "invalid-context-summary"],
    [async () => "x".repeat(5000), "context-summary-budget"],
    [async () => "x".repeat(1600), "context-budget"],
  ] as const) {
    await expect(projectContext(history(), options({ mode: "summarize", summarize }))).rejects.toMatchObject({ code, message: code });
  }
});

test("abort and timeout stop waiting for an uncooperative summarizer", async () => {
  for (const abort of [true, false]) {
    const controller = new AbortController();
    let summarySignal: AbortSignal | undefined;
    const promise = projectContext(history(), options({ mode: "summarize", summaryTimeoutMs: 10,
      summarize: async input => {
        summarySignal = input.signal;
        if (abort) controller.abort();
        return new Promise<string>(() => {});
      },
    }), controller.signal);
    await expect(promise).rejects.toMatchObject({ code: abort ? "cancelled" : "context-summary-timeout" });
    expect(summarySignal?.aborted).toBe(true);
  }
  const controller = new AbortController(); controller.abort();
  await expect(projectContext(history(), options(), controller.signal)).rejects.toMatchObject({ code: "cancelled" });
});

test("UTF-8 budgets include multibyte text and exact token reserve boundaries", async () => {
  const messages = [user("中文约束"), assistant("完成"), user("继续")];
  const exact = size(messages);
  const out = await projectContext(messages, options({ budget: { maxBytes: exact, maxTokens: exact + 100, reserveTokens: 100, estimateTokens: size } }));
  expect(out.report.mode).toBe("unchanged");
  expect(out.report.beforeBytes).toBe(exact);
  const trimmed = await projectContext(messages, options({ budget: { maxBytes: exact - 1, maxTokens: exact + 100, reserveTokens: 100, estimateTokens: size } }));
  expect(trimmed.report.removedIndices).toEqual([1]);
  const tokenTrim = await projectContext(messages, options({ budget: { maxBytes: exact, maxTokens: exact + 99, reserveTokens: 100, estimateTokens: size } }));
  expect(tokenTrim.report.removedIndices).toEqual([1]);
});

test("invalid options and broken or mutating estimators cannot bypass budgets", async () => {
  for (const budget of [
    { ...options().budget, maxTokens: NaN }, { ...options().budget, reserveTokens: 1800 },
    { ...options().budget, maxBytes: 0 },
  ]) await expect(projectContext(history(), options({ budget }))).rejects.toMatchObject({ code: "invalid-context-options" });
  for (const estimateTokens of [() => NaN, () => -1, () => 1.5, () => { throw Error("private"); }])
    await expect(projectContext(history(), options({ budget: { ...options().budget, estimateTokens } }))).rejects.toMatchObject({ code: "context-estimate-failed" });
  const messages = [user()];
  const out = await projectContext(messages, options({ budget: { ...options().budget, estimateTokens: copy => { (copy[0] as { content: string }).content = "changed"; return 100; } } }));
  expect(out.messages).toEqual(messages);
  await expect(projectContext(messages, options({ protectedIndices: [9] }))).rejects.toMatchObject({ code: "invalid-context-options" });
});

test("adapter has no cross-call summary cache or transcript mutation", async () => {
  const reports: ContextProjection["report"][] = [];
  const transform = createContextTransform(options({ mode: "summarize", summarize: async () => "Brief." }), report => { reports.push(report); report.removedIndices.push(999); });
  const first = history();
  const second = [user("A different session.")];
  const [a, b] = await Promise.all([transform(first), transform(second)]);
  expect(a[0]).toEqual(first[0]);
  expect(b).toEqual(second);
  expect(reports).toHaveLength(2);
  expect(first).toEqual(history());
});

test("long transcripts retain every constraint including multimodal user content", async () => {
  const messages: AgentMessage[] = Array.from({ length: 100 }, (_, index) => [
    user(`Constraint ${index}: keep local.`), assistant(),
  ]).flat();
  messages.push({ role: "user", content: [
    { type: "text", text: "Respect all previous constraints." },
    { type: "image", data: "ZmFrZQ==", mimeType: "image/png" },
  ], timestamp: 99 });
  const out = await projectContext(messages, options({ mode: "summarize",
    budget: { maxTokens: 14100, reserveTokens: 100, maxBytes: 14000, estimateTokens: size },
    summarize: async input => { expect(input.messages).toHaveLength(100); return "100 completed assistant explanations."; },
  }));
  expect(out.report.removedIndices).toHaveLength(100);
  expect(out.messages.filter(message => message.role === "user").slice(2))
    .toEqual(messages.filter(message => message.role === "user").slice(1));
  expect(out.messages[0]).toEqual(messages[0]);
  expect(out.report.afterBytes).toBeLessThanOrEqual(14000);
});

test("Pi adapter compacts requests, preserves live tool pairs and leaves durable history intact", async () => {
  const core = createFauxCore({ models: [{ id: "offline" }], tokensPerSecond: Infinity });
  const store = createMemoryStore();
  const identity = { subject: "alice", scope: "test", application: "context", target: "local" };
  const reports: ContextProjection["report"][] = [];
  const definition = { name: "lookup", description: "Local fixture", inputSchema: { type: "object" },
    target: { application: "context", instance: "local", plugin: "test", operation: "lookup" }, effect: "read" as const, confirmation: false };
  let invocations = 0;
  const agent = createAgentService({ store, identity: async () => identity, model: core.getModel(),
    profiles: [{ id: "default", instructions: "Keep constraints.", tools: ["lookup"] }],
    tools: { catalog: async () => [definition], prepare: async (_name, input) => ({ definition, input, precondition: null }),
      invoke: async () => { invocations++; return { found: true }; } },
  }, { streamFn: core.streamSimple, transformContext: createContextTransform(options({
    mode: "summarize", keepRecentMessages: 1,
    budget: { maxTokens: 3100, reserveTokens: 100, maxBytes: 3000, estimateTokens: size },
    summarize: async () => "Earlier assistant described a local item.",
  }), report => reports.push(report)) });
  try {
    const session = await agent.createSession(identity, "default");
    core.setResponses([fauxAssistantMessage("old detail ".repeat(400))]);
    expect((await (await agent.startRun(identity, session.id, "Never deploy or spend money.")).done).status).toBe("completed");
    const before = await agent.readSession(identity, session.id);
    core.setResponses([context => {
      expect(size(context.messages)).toBeLessThanOrEqual(3000);
      expect(JSON.stringify(context.messages)).toContain("Never deploy or spend money.");
      expect(JSON.stringify(context.messages)).toContain("untrusted reference data");
      return fauxAssistantMessage(fauxToolCall("lookup", {}));
    }, context => {
      expect(context.messages.some(message => message.role === "toolResult")).toBe(true);
      const toolMessage = context.messages.find(message => message.role === "assistant" && message.content.some(part => part.type === "toolCall"));
      expect(toolMessage).toBeDefined();
      return fauxAssistantMessage("Done locally.");
    }]);
    expect((await (await agent.startRun(identity, session.id, "Continue in Chinese.")).done).status).toBe("completed");
    const after = await agent.readSession(identity, session.id);
    expect(after.messages.slice(0, before.messages.length)).toEqual(before.messages);
    expect(after.messages.some(message => JSON.stringify(message.value).includes("Historical summary"))).toBe(false);
    expect(reports.some(report => report.mode === "summarize")).toBe(true);
    expect({ invocations, calls: after.calls, results: after.messages.filter(message => message.value.role === "toolResult") }).toMatchObject({ invocations: 1 });
  } finally { await agent.close(); store.close(); }
});
