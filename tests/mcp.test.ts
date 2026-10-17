import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createMcpStdioClient, createMcpToolSource, createMcpToolPlugin, type McpStdioOptions } from "../src/mcp";
import { combineToolSources } from "../src/tools";
import { createAgentService, createMemoryStore } from "../src/index";
import { startApp, definePlugin } from "@lenso/core";
import { createFauxCore, fauxAssistantMessage, fauxToolCall } from "@earendil-works/pi-ai";
import type { Identity } from "../src/types";

const signal = new AbortController().signal;
const identity: Identity = { subject: "alice", scope: "test", application: "test", target: "local" };
const owned: { close(): Promise<void> }[] = [];
const directories: string[] = [];
afterEach(async () => {
  await Promise.all(owned.splice(0).map(item => item.close()));
  for (const path of directories.splice(0)) rmSync(path, { recursive: true, force: true });
});
function server(mode = "normal", log?: string): McpStdioOptions {
  return { command: process.execPath, args: [join(import.meta.dir, "fixtures/mcp-server.ts"), mode, ...(log ? [log] : [])], timeoutMs: 2000 };
}
function client(mode = "normal", overrides: Partial<McpStdioOptions> = {}) {
  const value = createMcpStdioClient({ ...server(mode), ...overrides });
  owned.push(value);
  return value;
}
function source(sourceId = "mcp-one") {
  let allowed = true;
  const value = createMcpToolSource<Identity>({
    application: "test", instance: "local", sourceId, server: server(),
    selections: [
      { name: "read_echo", tool: "echo", effect: "read" },
      { name: "write_change", tool: "change" },
      { name: "future_added", tool: "added", effect: "read" },
      { name: "old_removed", tool: "removed", effect: "read" },
    ],
    authorize: async (_selection, _input, context) => allowed && context.subject === "alice",
    validateInput: (input, schema) => {
      if (schema.type !== "object" || (input.value !== undefined && typeof input.value !== "string")) throw new Error("private validation detail");
    },
  });
  owned.push(value);
  return { value, revoke: () => { allowed = false; } };
}

test("stdio handshake, full pagination, structured content and tool errors survive without inherited secrets", async () => {
  process.env.LENSO_MCP_TEST_SECRET = "not-a-real-secret";
  const c = client();
  delete process.env.LENSO_MCP_TEST_SECRET;
  const catalog = await c.listTools(signal);
  expect(catalog.tools.map(tool => tool.name)).toEqual(["echo", "change", "hang", "stats", "crash", "error", "malformed", "removed"]);
  const result = await c.callTool("echo", { value: "hello" }, signal, catalog.revision);
  expect(result.content).toEqual([{ type: "text", text: "fallback" }]);
  expect(result.structuredContent?.value).toBe("hello");
  expect(await c.callTool("error", {}, signal, catalog.revision)).toEqual({ content: [{ type: "text", text: "business failure" }], structuredContent: { ok: false }, isError: true });
  const stats = await c.callTool("stats", {}, signal, catalog.revision);
  expect(stats.structuredContent?.lists).toBe(4);
  expect(stats.structuredContent?.inherited).toBeNull();
});

test("list change invalidates old calls and replaces an entire paginated snapshot", async () => {
  const c = client();
  const before = await c.listTools(signal);
  await c.callTool("change", {}, signal, before.revision);
  await expect(c.callTool("echo", {}, signal, before.revision)).rejects.toThrow("tool-conflict");
  const after = await c.listTools(signal);
  expect(after.revision).toBeGreaterThan(before.revision);
  expect(after.tools.map(tool => tool.name)).toContain("added");
  expect(after.tools.map(tool => tool.name)).not.toContain("removed");
  const during = await client("page-change").listTools(signal);
  expect(during.tools).toHaveLength(8);
  expect(during.tools.every(tool => tool.description?.endsWith(" updated"))).toBe(true);
});

test("discovery rejects cycles, duplicates, oversized frames, endless changes and unsupported protocol", async () => {
  for (const [mode, code] of [["cycle", "mcp-protocol"], ["duplicate", "mcp-protocol"], ["churning", "tool-conflict"], ["bad-version", "mcp-protocol"]]) {
    await expect(client(mode).listTools(signal)).rejects.toThrow(code);
  }
  await expect(client("oversized", { maxMessageBytes: 4096 }).listTools(signal)).rejects.toThrow("mcp-disconnected");
  await expect(client("normal", { maxPages: 1 }).listTools(signal)).rejects.toThrow("output-budget");
  await expect(client("normal", { maxTools: 1 }).listTools(signal)).rejects.toThrow("output-budget");
});

test("timeouts send cancellation, ignore late replies and retain a usable connection", async () => {
  const c = client("normal", { timeoutMs: 150 });
  const catalog = await c.listTools(signal);
  await expect(c.callTool("hang", {}, signal, catalog.revision)).rejects.toThrow("outcome-unknown");
  const stats = await c.callTool("stats", {}, signal, catalog.revision);
  expect(stats.structuredContent?.cancellations).toBe(1);
  await expect(client("list-hang", { timeoutMs: 150 }).listTools(signal)).rejects.toThrow("mcp-timeout");
});

test("caller cancellation stops waiting, cancels the remote request and does not affect other calls", async () => {
  const c = client();
  const catalog = await c.listTools(signal), abort = new AbortController();
  const waiting = c.callTool("hang", {}, abort.signal, catalog.revision);
  const concurrent = await c.callTool("echo", { value: "other" }, signal, catalog.revision);
  abort.abort();
  await expect(waiting).rejects.toThrow("outcome-unknown");
  expect(concurrent.structuredContent?.value).toBe("other");
  expect((await c.callTool("stats", {}, signal, catalog.revision)).structuredContent?.cancellations).toBe(1);
  await expect(c.callTool("echo", {}, abort.signal, catalog.revision)).rejects.toThrow("cancelled");
});

test("disconnect never replays effects; next discovery reconnects and old preparations stay invalid", async () => {
  const dir = mkdtempSync(join(tmpdir(), "lenso-mcp-")); directories.push(dir);
  const log = join(dir, "calls.txt"), c = client("normal", server("normal", log));
  const first = await c.listTools(signal);
  await expect(c.callTool("crash", {}, signal, first.revision)).rejects.toThrow("outcome-unknown");
  const next = await c.listTools(signal);
  expect(next.revision).toBeGreaterThan(first.revision);
  await expect(c.callTool("echo", {}, signal, first.revision)).rejects.toThrow("tool-conflict");
  await c.callTool("echo", {}, signal, next.revision);
  expect(readFileSync(log, "utf8")).toBe("crash\necho\n");
  await c.close();
  await c.close();
  await expect(c.listTools(signal)).rejects.toThrow("mcp-closed");
});

test("shutdown aborts initialization and outstanding calls and reaps owned processes", async () => {
  const c = client("init-hang");
  const initializing = c.listTools(signal);
  const failed = initializing.catch(error => error.message);
  await c.close(); expect(await failed).toBe("mcp-disconnected");
  const live = client(), catalog = await live.listTools(signal);
  const pid = (await live.callTool("stats", {}, signal, catalog.revision)).structuredContent!.pid as number;
  const hanging = live.callTool("hang", {}, signal, catalog.revision).catch(error => error.message);
  await live.close(); expect(await hanging).toBe("outcome-unknown");
  expect(() => process.kill(pid, 0)).toThrow();
});

test("cancelled initialization waiters do not cancel shared discovery; spawn failures settle safely", async () => {
  const c = client("init-delay"), abort = new AbortController();
  const cancelled = c.listTools(abort.signal).catch(error => error.message);
  const survivor = c.listTools(signal);
  abort.abort();
  expect(await cancelled).toBe("cancelled");
  const catalog = await survivor;
  expect(catalog.tools).toHaveLength(8);
  await expect(c.callTool("malformed", {}, signal, catalog.revision)).rejects.toThrow("outcome-unknown");
  const missing = client("normal", { command: "/nonexistent-lenso-mcp-fixture" });
  await expect(missing.listTools(signal)).rejects.toThrow("mcp-disconnected");
  await expect(missing.listTools(signal)).rejects.toThrow("mcp-disconnected");
});

test("aliases, host policy, schema validation, identity and reauthorization stay isolated", async () => {
  const { value: s, revoke } = source();
  const catalog = await s.catalog(identity, identity, signal);
  expect(catalog.map(tool => tool.name)).toEqual(["read_echo", "write_change", "old_removed"]);
  expect(catalog[1]!.effect).toBe("unknown");
  expect(catalog[1]!.confirmation).toBe(true);
  expect(catalog[0]!.target).toEqual({ application: "test", instance: "local", plugin: "mcp-one", operation: "echo" });
  expect(await s.catalog({ ...identity, subject: "bob" }, identity, signal)).toEqual([]);
  await expect(s.catalog(identity, { ...identity, target: "elsewhere" }, signal)).rejects.toThrow("forbidden-operation");
  await expect(s.prepare("echo", {}, identity, identity, signal)).rejects.toThrow("unknown-operation");
  await expect(s.prepare("read_echo", { value: 42 }, identity, identity, signal)).rejects.toThrow("invalid-input");
  const prepared = await s.prepare("read_echo", { value: "valid" }, identity, identity, signal);
  expect((await s.invoke(prepared, identity, identity, signal, false) as any).structuredContent.value).toBe("valid");
  const change = await s.prepare("write_change", {}, identity, identity, signal);
  await expect(s.invoke(change, identity, identity, signal, false)).rejects.toThrow("confirmation-required");
  await s.invoke(change, identity, identity, signal, true);
  expect((await s.catalog(identity, identity, signal)).map(tool => tool.name)).toEqual(["read_echo", "write_change", "future_added"]);
  await expect(s.invoke(prepared, identity, identity, signal, false)).rejects.toThrow("tool-conflict");
  const fresh = await s.prepare("read_echo", {}, identity, identity, signal);
  revoke();
  await expect(s.invoke(fresh, identity, identity, signal, false)).rejects.toThrow("forbidden-operation");
});

test("composition reserves unavailable aliases and prevents cross-source prepared-tool substitution", async () => {
  const a = source("one").value, b = source("two").value;
  const combined = combineToolSources([{ source: a, tools: a.tools }]);
  expect((await combined.catalog(identity, identity, signal)).map(tool => tool.name)).not.toContain("future_added");
  expect(() => combineToolSources([{ source: a, tools: a.tools }, { source: b, tools: ["future_added"] }])).toThrow("duplicate-tool");
  const prepared = await a.prepare("read_echo", {}, identity, identity, signal);
  await expect(b.invoke(prepared, identity, identity, signal, false)).rejects.toThrow("tool-conflict");
  expect(createMcpToolPlugin({ id: "mcp-test", options: () => { throw new Error("setup only"); } }).id).toBe("mcp-test");
});

test("actual Agent gateway saves MCP structured results and admits writes only after confirmation", async () => {
  const s = source().value, core = createFauxCore({ models: [{ id: "offline-mcp" }] });
  core.setResponses([
    fauxAssistantMessage(fauxToolCall("read_echo", { value: "through-agent" })),
    context => {
      const result = context.messages.findLast(message => message.role === "toolResult");
      const content = result?.role === "toolResult" && result.content[0];
      expect(content && content.type === "text" && JSON.parse(content.text).structuredContent.value).toBe("through-agent");
      return fauxAssistantMessage(fauxToolCall("write_change", {}));
    },
    fauxAssistantMessage("Waiting for confirmation."),
  ]);
  const agent = createAgentService({
    store: createMemoryStore(), identity: async (context: Identity) => context,
    tools: combineToolSources([{ source: s, tools: s.tools }]), model: core.getModel(),
    profiles: [{ id: "mcp", instructions: "Local tools only.", tools: s.tools }],
  }, { streamFn: core.streamSimple });
  try {
    const session = await agent.createSession(identity, "mcp");
    expect((await (await agent.startRun(identity, session.id, "Read then change.")).done).status).toBe("completed");
    const saved = await agent.readSession(identity, session.id);
    expect(saved.calls[0]?.status).toBe("completed");
    expect((saved.calls[0]?.result as any).structuredContent.value).toBe("through-agent");
    expect(saved.actions[0]?.status).toBe("pending");
    expect((await s.catalog(identity, identity, signal)).map(tool => tool.name)).toContain("old_removed");
    const action = saved.actions[0]!;
    expect((await agent.confirmAction(identity, action.id, action.digest)).status).toBe("completed");
    expect((await s.catalog(identity, identity, signal)).map(tool => tool.name)).toContain("future_added");
  } finally { await agent.close(); }
});

test("optional plugin owns child cleanup on app stop and failed downstream setup", async () => {
  const plugin = createMcpToolPlugin<Identity>({ id: "mcp", options: () => ({
    application: "test", instance: "local", sourceId: "plugin-source", server: server(),
    selections: [{ name: "stats", tool: "stats", effect: "read" }],
    authorize: async () => true, validateInput: () => {},
  }) });
  let pid = 0;
  async function readPid(s: ReturnType<typeof source>["value"]) {
    const prepared = await s.prepare("stats", {}, identity, identity, signal);
    pid = (await s.invoke(prepared, identity, identity, signal, false) as any).structuredContent.pid;
  }
  const app = await startApp({ instanceId: "local", plugins: [plugin] });
  try { await readPid(app.get(plugin)); } finally { await app.stop(); }
  expect(() => process.kill(pid, 0)).toThrow();
  const failing = definePlugin({ id: "fails", requires: [plugin], async setup(context) {
    await readPid(context.get(plugin));
    throw new Error("expected-failure");
  } });
  await expect(startApp({ instanceId: "local", plugins: [plugin, failing] })).rejects.toThrow("expected-failure");
  expect(() => process.kill(pid, 0)).toThrow();
});
