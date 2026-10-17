import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createMcpStdioClient, createMcpToolSource, type McpStdioOptions, type McpToolSourceOptions } from "../src/mcp";
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
function client(mode: string, log?: string) {
  const value = createMcpStdioClient(server(mode, log));
  owned.push(value);
  return value;
}
function callLog() {
  const dir = mkdtempSync(join(tmpdir(), "lenso-mcp-discovery-"));
  directories.push(dir);
  return join(dir, "calls.txt");
}
function source(overrides: Partial<McpToolSourceOptions<Identity>> = {}) {
  const value = createMcpToolSource<Identity>({
    application: "test", instance: "local", sourceId: "discovery-test", server: server(),
    selections: [{ name: "local_echo", tool: "echo", effect: "read" }],
    authorize: async () => true, validateInput: () => {}, ...overrides,
  });
  owned.push(value);
  return value;
}

for (const mode of ["silent-input", "silent-output", "silent-catalog"]) {
  test(`${mode}: a refreshed catalog fences old revisions without replaying calls`, async () => {
    const log = callLog(), c = client(mode, log);
    const before = await c.listTools(signal);
    await c.callTool("change", {}, signal, before.revision);
    const after = await c.listTools(signal);
    expect(after.tools).not.toEqual(before.tools);
    expect(after.revision).toBeGreaterThan(before.revision);
    await expect(c.callTool("echo", {}, signal, before.revision)).rejects.toThrow("tool-conflict");
    expect((await c.listTools(signal)).revision).toBe(after.revision);
    await c.callTool("echo", {}, signal, after.revision);
    // Returning to an earlier schema must not resurrect its old admission.
    await c.callTool("change", {}, signal, after.revision);
    const restored = await c.listTools(signal);
    expect(restored.tools).toEqual(before.tools);
    expect(restored.revision).toBeGreaterThan(after.revision);
    await expect(c.callTool("echo", {}, signal, before.revision)).rejects.toThrow("tool-conflict");
    expect(readFileSync(log, "utf8")).toBe("change\necho\nchange\n");
  });
}

test("catalog comparison ignores page order and caller mutation, but always rediscovers", async () => {
  const c = client("silent-order"), before = await c.listTools(signal);
  const original = structuredClone(before.tools);
  before.tools[0]!.inputSchema = { type: "object", required: ["caller-only"] };
  const refreshed = await c.listTools(signal);
  expect(refreshed.tools).toEqual(original);
  expect(refreshed.revision).toBe(before.revision);
  await c.callTool("change", {}, signal, refreshed.revision);
  const reordered = await c.listTools(signal);
  expect(reordered.tools).toEqual(original.toReversed());
  expect(reordered.revision).toBe(before.revision);
  expect((await c.callTool("stats", {}, signal, reordered.revision)).structuredContent?.lists).toBe(12);
});

test("duplicate names across pages reject the entire discovery", async () => {
  await expect(client("duplicate-pages").listTools(signal)).rejects.toThrow("mcp-protocol");
});

test("failed partial refresh exposes a safe error and cannot replace the complete snapshot", async () => {
  const c = client("silent-partial-error"), before = await c.listTools(signal);
  await c.callTool("change", {}, signal, before.revision);
  const error = await c.listTools(signal).catch(error => error);
  expect(error.message).toBe("operation-failed");
  expect(error.cause).toBeUndefined();
  // A later explicit discovery must still compare against the old complete snapshot.
  const after = await c.listTools(signal);
  expect(after.revision).toBeGreaterThan(before.revision);
  expect(after.tools).toHaveLength(8);
  expect(after.tools.every(tool => tool.description?.endsWith(" updated"))).toBe(true);
  await expect(c.callTool("echo", {}, signal, before.revision)).rejects.toThrow("tool-conflict");
});

test("JSON-RPC invocation errors remain outcome-unknown and are never replayed", async () => {
  const log = callLog(), c = client("rpc-error", log), catalog = await c.listTools(signal);
  const error = await c.callTool("echo", {}, signal, catalog.revision).catch(error => error);
  expect(error.message).toBe("outcome-unknown");
  expect(error.cause).toBeUndefined();
  expect(readFileSync(log, "utf8")).toBe("echo\n");
});

test("concurrent refreshes converge on one revision after a silent schema change", async () => {
  const c = client("silent-input"), before = await c.listTools(signal);
  await c.callTool("change", {}, signal, before.revision);
  const catalogs = await Promise.all([c.listTools(signal), c.listTools(signal), c.listTools(signal)]);
  expect(catalogs[0]!.revision).toBeGreaterThan(before.revision);
  expect(new Set(catalogs.map(catalog => catalog.revision)).size).toBe(1);
  for (const catalog of catalogs) {
    expect(catalog.tools).toHaveLength(8);
    expect(catalog.tools[0]!.inputSchema).toEqual({ type: "object", properties: { value: { type: "number" } } });
  }
});

test("source keeps stable aliases and rejects cached preparations after a silent schema change", async () => {
  const log = callLog(), s = source({ server: server("silent-input", log), selections: [
    { name: "local_echo", tool: "echo", effect: "read" }, { name: "local_change", tool: "change", effect: "write" },
  ] });
  const old = await s.prepare("local_echo", {}, identity, identity, signal);
  const change = await s.prepare("local_change", {}, identity, identity, signal);
  await s.invoke(change, identity, identity, signal, true);
  await expect(s.invoke(old, identity, identity, signal, false)).rejects.toThrow("tool-conflict");
  expect((await s.catalog(identity, identity, signal)).map(tool => tool.name)).toEqual(["local_echo", "local_change"]);
  const fresh = await s.prepare("local_echo", {}, identity, identity, signal);
  await s.invoke(fresh, identity, identity, signal, false);
  expect(readFileSync(log, "utf8")).toBe("change\necho\n");
});

for (const stage of ["visibility", "validation", "input-authorization"] as const) {
  test(`cancellation during ${stage} stops admission at that boundary`, async () => {
    const entered = Promise.withResolvers<void>(), release = Promise.withResolvers<void>();
    let authorizations = 0, validations = 0;
    const s = source({
      authorize: async (_selection, input) => {
        authorizations++;
        if ((stage === "visibility" && input === undefined) || (stage === "input-authorization" && input !== undefined)) {
          entered.resolve(); await release.promise; return false;
        }
        return true;
      },
      validateInput: async () => {
        validations++;
        if (stage === "validation") { entered.resolve(); await release.promise; }
      },
    });
    const abort = new AbortController();
    const pending = s.prepare("local_echo", {}, identity, identity, abort.signal).catch(error => error.message);
    await entered.promise;
    abort.abort(); release.resolve();
    expect(await pending).toBe("cancelled");
    expect(authorizations).toBe(stage === "input-authorization" ? 2 : 1);
    expect(validations).toBe(stage === "visibility" ? 0 : 1);
  });
}
