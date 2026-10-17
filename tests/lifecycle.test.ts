import { expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createFauxCore, fauxAssistantMessage } from "@earendil-works/pi-ai";
import { createAgentService } from "../src/runtime";
import { createMemoryStore } from "../src/store";
import { createSqliteStore } from "../src/sqlite";
import { createAgentFetchHandler } from "../src/fetch";
import type { AgentStore } from "../src/types";

const identity = { subject: "alice", scope: "test", application: "local", target: "fixture" };
function deferred<T = void>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
function fixture(store: AgentStore = createMemoryStore()) {
  const core = createFauxCore({ models: [{ id: "offline" }] });
  const service = createAgentService({
    store, model: core.getModel(), identity: async (context: typeof identity) => context,
    profiles: [{ id: "default", instructions: "Offline only", tools: [] }],
    tools: { async catalog() { return []; }, async prepare() { throw new Error("unused"); }, async invoke() { throw new Error("unused"); } },
  }, { streamFn: core.streamSimple });
  return { core, service, store };
}

test("idempotent admission shares running work, survives reopen, and rejects changed input or identity", async () => {
  const f = fixture();
  const entered = deferred();
  const release = deferred();
  f.core.setResponses([async () => { entered.resolve(); await release.promise; return fauxAssistantMessage("Once"); }]);
  const session = await f.service.createSession(identity, "default");
  const first = await f.service.startRun(identity, session.id, "Hello", { requestId: "click-1" });
  await entered.promise;
  const retry = await f.service.startRun(identity, session.id, "Hello", { requestId: "click-1" });
  expect(retry.run.id).toBe(first.run.id);
  expect(retry.done).toBe(first.done);
  await expect(f.service.startRun(identity, session.id, "Changed", { requestId: "click-1" })).rejects.toThrow("request-id-conflict");
  await expect(f.service.startRun({ ...identity, subject: "mallory" }, session.id, "Hello", { requestId: "click-1" })).rejects.toThrow("not-found");
  const other = fixture(f.store);
  await expect(other.service.startRun(identity, session.id, "Hello", { requestId: "click-1" })).rejects.toThrow("run-executor-unavailable");
  await expect(other.service.requestCancelRun(identity, first.run.id)).rejects.toThrow("run-executor-unavailable");
  release.resolve();
  await first.done;
  await f.service.close();
  const restored = await other.service.startRun(identity, session.id, "Hello", { requestId: "click-1" });
  expect((await restored.done).status).toBe("completed");
  expect(other.core.state.callCount).toBe(0);
  expect(f.core.state.callCount).toBe(1);
  expect((await other.service.readSession(identity, session.id)).messages.filter(m => m.value.role === "user")).toHaveLength(1);
  for (const requestId of ["", "x".repeat(129), "with spaces"]) {
    await expect(other.service.startRun(identity, session.id, "Hello", { requestId })).rejects.toThrow("invalid-request-id");
  }
  await other.service.close();
});

test("Fetch accepts durable cancellation before an uncooperative executor drains and deduplicates retries", async () => {
  const entered = deferred();
  const release = deferred();
  const f = fixture();
  f.core.setResponses([async () => { entered.resolve(); await release.promise; return fauxAssistantMessage("Finished late"); }]);
  const handler = createAgentFetchHandler({ agent: f.service, authenticate: async () => identity });
  const session = await f.service.createSession(identity, "default");
  const request = (path: string, body: object, headers = {}) => handler(new Request(`http://local/agent/${path}`, {
    method: "POST", headers: { "content-type": "application/json", ...headers }, body: JSON.stringify(body),
  }));
  const start = await request(`sessions/${session.id}/runs`, { text: "Hello" }, { "idempotency-key": "click-2" });
  const run = await start!.json();
  await entered.promise;
  const duplicate = await request(`sessions/${session.id}/runs`, { text: "Hello" }, { "idempotency-key": "click-2" });
  expect((await duplicate!.json()).id).toBe(run.id);
  const conflict = await request(`sessions/${session.id}/runs`, { text: "Changed" }, { "idempotency-key": "click-2" });
  expect(conflict!.status).toBe(409);
  const response = await request(`runs/${run.id}/cancel`, {});
  expect(response!.status).toBe(202);
  const requested = await response!.json();
  expect(requested.status).toBe("running");
  expect(requested.cancelRequestedAt).toBeNumber();
  expect((await f.service.requestCancelRun(identity, run.id)).cancelRequestedAt).toBe(requested.cancelRequestedAt);
  await expect(f.service.startRun(identity, session.id, "New")).rejects.toThrow("session-busy");
  release.resolve();
  const drained = await f.service.cancelRun(identity, run.id);
  expect(drained.status).toBe("cancelled");
  expect(drained.cancelRequestedAt).toBe(requested.cancelRequestedAt);
  expect((await request(`runs/${run.id}/cancel`, {}))!.status).toBe(200);
  await f.service.close();
});

test("shutdown persists intent, and a failed cancellation receipt still aborts and drains work", async () => {
  for (const failReceipt of [false, true]) {
    const backing = createMemoryStore();
    const entered = deferred();
    const release = deferred();
    const store: AgentStore = {
      transaction: work => backing.transaction(tx => work({
        ...tx,
        put(table, record) {
          if (failReceipt && table === "runs" && "cancelRequestedAt" in record && record.status === "running")
            throw new Error("private-storage-failure");
          tx.put(table, record);
        },
      })),
      close: () => backing.close(),
    };
    const f = fixture(store);
    f.core.setResponses([async request => { entered.resolve(); await release.promise; return fauxAssistantMessage("Late"); }]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Hello");
    await entered.promise;
    let settled = false;
    const closing = f.service.close();
    void closing.then(() => { settled = true; }, () => { settled = true; });
    await Bun.sleep(0);
    expect(settled).toBe(false);
    const saved = backing.transaction(tx => tx.get("runs", handle.run.id));
    expect(saved?.status).toBe("running");
    expect(saved?.cancelRequestedAt !== undefined).toBe(!failReceipt);
    release.resolve();
    expect((await handle.done).status).toBe("cancelled");
    if (failReceipt) await expect(closing).rejects.toThrow("cancel-receipt-unavailable");
    else await closing;
  }
});

test("SIGKILL preserves cancellation intent; recovery and retry never repeat an entered local effect", async () => {
  const dir = mkdtempSync(join(tmpdir(), "agent-lifecycle-"));
  const path = join(dir, "fixture.sqlite");
  const child = Bun.spawn([process.execPath, new URL("./fixtures/lifecycle-child.ts", import.meta.url).pathname, path], {
    stdout: "pipe", stderr: "pipe",
  });
  let db: Database | undefined;
  let service: ReturnType<typeof fixture>["service"] | undefined;
  try {
    const reader = child.stdout.getReader();
    const first = await reader.read();
    expect(first.done).toBe(false);
    const ids = JSON.parse(new TextDecoder().decode(first.value));
    child.kill("SIGKILL");
    await child.exited;
    await reader.cancel();
    expect(await new Response(child.stderr).text()).toBe("");
    db = new Database(path);
    const store = createSqliteStore(db);
    const f = fixture(store);
    service = f.service;
    expect((await service.getRun(identity, ids.runId)).cancelRequestedAt).toBeNumber();
    await expect(service.startRun(identity, ids.sessionId, "One local effect", { requestId: "kill-test" })).rejects.toThrow("run-executor-unavailable");
    await service.recoverInterrupted();
    await service.recoverInterrupted();
    const retry = await service.startRun(identity, ids.sessionId, "One local effect", { requestId: "kill-test" });
    expect((await retry.done).status).toBe("interrupted");
    const snapshot = await service.readSession(identity, ids.sessionId);
    expect(snapshot.calls[0].status).toBe("outcome_unknown");
    expect(snapshot.runs).toHaveLength(1);
    expect(db.query("SELECT count(*) AS n FROM fixture_effects").get()).toEqual({ n: 1 });
    expect(f.core.state.callCount).toBe(0);
    f.core.setResponses([fauxAssistantMessage("New user request works")]);
    expect((await (await service.startRun(identity, ids.sessionId, "Continue explicitly", { requestId: "new-click" })).done).status).toBe("completed");
  } finally {
    child.kill("SIGKILL");
    await child.exited;
    await service?.close();
    db?.close();
    rmSync(dir, { recursive: true, force: true });
  }
}, 10_000);
