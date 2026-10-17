import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createMcpStdioClient, createMcpToolSource, type McpStdioOptions } from "../src/mcp";
import type { Identity } from "../src/types";

const signal = new AbortController().signal;
const identity: Identity = { subject: "alice", scope: "test", application: "test", target: "local" };
function alive(pid: number): boolean {
  try { process.kill(pid, 0); return true; } catch { return false; }
}
async function within<T>(promise: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  try {
    return await Promise.race([promise, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error("integration-deadline")), 2000);
    })]);
  } finally { clearTimeout(timer!); }
}
function fixture() {
  const dir = mkdtempSync(join(tmpdir(), "lenso-mcp-integration-")), log = join(dir, "calls.txt");
  const server: McpStdioOptions = {
    command: process.execPath,
    args: [join(import.meta.dir, "fixtures/mcp-discovery-tree.ts"), "silent-input", log, join(dir, "worker")],
    timeoutMs: 1000,
  };
  const processes = () => readdirSync(dir).filter(file => file.startsWith("worker.")).flatMap(file => [
    Number(file.slice("worker.".length)), Number(readFileSync(join(dir, file), "utf8")),
  ]);
  return {
    server, log, processes,
    async cleanup(owner: { close(): Promise<void> }) {
      // Only fixture-owned PIDs, including a worker left behind by a failed assertion.
      for (const pid of processes()) if (alive(pid)) process.kill(pid, "SIGKILL");
      await within(owner.close());
      rmSync(dir, { recursive: true, force: true });
    },
  };
}
async function expectStopped(pids: number[]) {
  for (let attempt = 0; pids.some(alive) && attempt < 100; attempt++) await Bun.sleep(5);
  expect(pids.length).toBeGreaterThan(0);
  expect(pids.filter(alive)).toEqual([]);
}

test.skipIf(process.platform === "win32")("paginated refresh and cancelled results stay fenced across process-tree reconnect and close", async () => {
  const f = fixture(), client = createMcpStdioClient(f.server);
  try {
    const before = await client.listTools(signal);
    await client.callTool("change", {}, signal, before.revision);
    const [after, concurrent] = await Promise.all([client.listTools(signal), client.listTools(signal)]);
    expect(after.tools).toHaveLength(8);
    expect(after.revision).toBeGreaterThan(before.revision);
    expect(concurrent.revision).toBe(after.revision);
    await expect(client.callTool("echo", {}, signal, before.revision)).rejects.toThrow("tool-conflict");

    const controller = new AbortController();
    const waiting = client.callTool("hang", {}, controller.signal, after.revision).catch(error => error.message);
    await client.callTool("stats", {}, signal, after.revision); // Dispatch barrier.
    controller.abort();
    expect(await waiting).toBe("outcome-unknown");
    expect((await client.callTool("stats", {}, signal, after.revision)).structuredContent!.cancellations).toBe(1);
    const previous = f.processes();
    await expect(client.callTool("crash", {}, signal, after.revision)).rejects.toThrow("outcome-unknown");
    const next = await client.listTools(signal);
    expect(next.tools).toHaveLength(8);
    expect(next.revision).toBeGreaterThan(after.revision);
    await expect(client.callTool("echo", {}, signal, after.revision)).rejects.toThrow("tool-conflict");
    await client.callTool("echo", {}, signal, next.revision);
    await expectStopped(previous);
    const hanging = client.callTool("hang", {}, signal, next.revision).catch(error => error.message);
    await client.callTool("stats", {}, signal, next.revision);
    await within(Promise.all([client.close(), client.close()]));
    expect(await hanging).toBe("outcome-unknown");
    await expectStopped(f.processes());
    expect(readFileSync(f.log, "utf8")).toBe("change\nhang\nstats\nstats\ncrash\necho\nhang\nstats\n");
  } finally { await f.cleanup(client); }
});

for (const stage of ["visibility", "validation", "input-authorization"] as const) {
  test.skipIf(process.platform === "win32")(`source close reaps workers while cancellation waits for ${stage}`, async () => {
    const f = fixture(), entered = Promise.withResolvers<void>(), release = Promise.withResolvers<void>();
    let blocking = false, authorizations = 0, validations = 0;
    const source = createMcpToolSource<Identity>({
      application: "test", instance: "local", sourceId: "integration", server: f.server,
      selections: [{ name: "read_echo", tool: "echo", effect: "read" }],
      authorize: async (_selection, input) => {
        if (blocking) {
          authorizations++;
          if ((stage === "visibility" && input === undefined) || (stage === "input-authorization" && input !== undefined)) {
            entered.resolve(); await release.promise; return false;
          }
        }
        return true;
      },
      validateInput: async () => {
        if (blocking) {
          validations++;
          if (stage === "validation") { entered.resolve(); await release.promise; }
        }
      },
    });
    try {
      const prepared = await source.prepare("read_echo", {}, identity, identity, signal);
      blocking = true;
      const controller = new AbortController();
      const pending = source.invoke(prepared, identity, identity, controller.signal, false).catch(error => error.message);
      await within(entered.promise);
      controller.abort();
      await within(source.close());
      await expectStopped(f.processes());
      release.resolve();
      expect(await within(pending)).toBe("cancelled");
      expect(authorizations).toBe(stage === "input-authorization" ? 2 : 1);
      expect(validations).toBe(stage === "visibility" ? 0 : 1);
      expect(() => readFileSync(f.log, "utf8")).toThrow(); // No tools/call was dispatched.
    } finally { release.resolve(); await f.cleanup(source); }
  });
}
