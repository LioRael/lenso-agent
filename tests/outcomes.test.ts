import { describe, expect, test, spyOn } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createFauxCore, fauxAssistantMessage, fauxToolCall } from "@earendil-works/pi-ai";
import { createAgentService } from "../src/runtime";
import { createMemoryStore } from "../src/store";
import { ensureActionOutcomes, isActionOutcomeMessage, persistActionOutcome } from "../src/outcomes";
import { AgentError, sameIdentity } from "../src/safety";
import type { AgentOptions, AgentStore, Identity, PendingAction, ToolDefinition, ToolSource } from "../src/types";

const identity: Identity = { subject: "alice", scope: "tenant-a", application: "test", target: "local" };
const write: ToolDefinition = {
  name: "update", description: "Offline simulated update",
  inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"], additionalProperties: false },
  target: { application: "test", instance: "local", plugin: "items", operation: "update" },
  effect: "write", confirmation: true,
};

function fixture(overrides: Partial<AgentOptions<Identity>> = {}) {
  const core = createFauxCore({ models: [{ id: "offline" }], tokenSize: { min: 1, max: 1 } });
  const store = overrides.store ?? createMemoryStore();
  const invocations: boolean[] = [];
  const tools: ToolSource<Identity> = {
    async catalog() { return [write]; },
    async prepare(_name, input) { return { definition: write, input, precondition: "v1" }; },
    async invoke(_prepared, _context, _identity, _signal, confirmed) {
      invocations.push(confirmed);
      return { receipt: "offline-confirmed-receipt" };
    },
  };
  const options: AgentOptions<Identity> = {
    store, tools, identity: async context => context, model: core.getModel(),
    profiles: [{ id: "default", instructions: "Use only listed tools.", tools: ["update"] }],
    ...overrides,
  };
  const service = createAgentService(options, { streamFn: core.streamSimple });
  async function propose(): Promise<PendingAction> {
    core.setResponses([
      fauxAssistantMessage(fauxToolCall("update", { id: "private-prepared-input" })),
      fauxAssistantMessage("Awaiting approval."),
    ]);
    const session = await service.createSession(identity, "default");
    expect((await (await service.startRun(identity, session.id, "Update item")).done).status).toBe("completed");
    return (await service.readSession(identity, session.id)).actions[0]!;
  }
  async function nextContext(sessionId: string): Promise<string> {
    let context = "";
    core.setResponses([request => {
      context = JSON.stringify(request.messages);
      return fauxAssistantMessage("Observed.");
    }]);
    expect((await (await service.startRun(identity, sessionId, "What happened?")).done).status).toBe("completed");
    return context;
  }
  return { core, store, tools, options, service, invocations, propose, nextContext };
}

function outcome(f: ReturnType<typeof fixture>, action: PendingAction) {
  const messages = f.store.transaction(tx => tx.list("messages", action.sessionId).filter(isActionOutcomeMessage));
  expect(messages).toHaveLength(1);
  const message = messages[0]!;
  expect(message.id).toBe(`action-outcome:${action.id}`);
  expect(message.source).toEqual({ kind: "action-outcome", actionId: action.id, callId: action.callId });
  expect(message.value.role).toBe("user");
  if (message.value.role !== "user" || typeof message.value.content !== "string") throw new Error("invalid outcome");
  return { message, fact: JSON.parse(message.value.content) };
}

describe("durable action outcomes in real offline Pi context", () => {
  test("human confirmation receipt is visible on the next model turn without executable arguments", async () => {
    const f = fixture();
    try {
      const action = await f.propose();
      const completed = await f.service.confirmAction(identity, action.id, action.digest);
      expect(completed.status).toBe("completed");
      expect(f.invocations).toEqual([true]);
      const context = await f.nextContext(action.sessionId);
      expect(context).toContain("offline-confirmed-receipt");
      expect(context).toContain(action.id);
      expect(context).not.toContain("private-prepared-input");
      expect(context).not.toContain('"toolCall"');
      expect(context).not.toContain('"prepared"');
      const saved = outcome(f, action);
      expect(saved.fact).toEqual({
        source: "action-outcome", actionId: action.id, callId: action.callId, runId: action.runId,
        modelCallId: f.store.transaction(tx => tx.get("calls", action.callId)!.modelCallId),
        toolName: "update", status: "completed", error: null,
        result: { receipt: "offline-confirmed-receipt" },
      });
      const before = JSON.stringify(saved.message);
      await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("action-completed");
      await f.service.readSession(identity, action.sessionId);
      await f.service.readSession(identity, action.sessionId);
      expect(JSON.stringify(outcome(f, action).message)).toBe(before);
      expect(f.invocations).toEqual([true]);
    } finally { await f.service.close(); }
  });

  test("repeated cancel and reads produce one cancelled fact, without invoking or claiming completion", async () => {
    const f = fixture();
    try {
      const action = await f.propose();
      expect((await f.service.cancelAction(identity, action.id)).status).toBe("cancelled");
      const first = JSON.stringify(outcome(f, action).message);
      expect((await f.service.cancelAction(identity, action.id)).status).toBe("cancelled");
      await f.service.readSession(identity, action.sessionId);
      expect(JSON.stringify(outcome(f, action).message)).toBe(first);
      expect(outcome(f, action).fact).toMatchObject({ status: "cancelled", error: "action-cancelled" });
      expect(outcome(f, action).fact).not.toHaveProperty("result");
      expect(await f.nextContext(action.sessionId)).toContain("action-cancelled");
      await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("action-cancelled");
      expect(f.invocations).toEqual([]);
    } finally { await f.service.close(); }
  });

  test("consumed audit rejection is durable; unconsumed preparation and authorization errors stay pending", async () => {
    let rejectExecution = false;
    const f = fixture({ audit: async fact => {
      if (rejectExecution && fact.kind === "tool_execution_admitted") throw new Error("private-audit-error");
    } });
    try {
      const action = await f.propose();
      const prepare = f.tools.prepare;
      f.tools.prepare = async () => { throw new AgentError("forbidden-operation"); };
      await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("forbidden-operation");
      await expect(f.service.confirmAction({ ...identity, scope: "other" }, action.id, action.digest)).rejects.toThrow("not-found");
      expect(f.store.transaction(tx => tx.get("actions", action.id)?.status)).toBe("pending");
      expect(f.store.transaction(tx => tx.list("messages", action.sessionId).filter(isActionOutcomeMessage))).toEqual([]);
      f.tools.prepare = prepare;
      rejectExecution = true;
      await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("operation-failed");
      const saved = outcome(f, action);
      expect(saved.fact).toMatchObject({ status: "rejected", error: "operation-failed" });
      expect(saved.fact).not.toHaveProperty("result");
      const context = await f.nextContext(action.sessionId);
      expect(context).toContain("rejected");
      expect(context).not.toContain("private-audit-error");
      expect(f.invocations).toEqual([]);
    } finally { await f.service.close(); }
  });

  test("post-invocation failure remains unknown, is visible next turn, and never grants a retry", async () => {
    const f = fixture();
    try {
      const action = await f.propose();
      f.tools.invoke = async () => {
        f.invocations.push(true);
        throw new Error("private-provider-error");
      };
      expect((await f.service.confirmAction(identity, action.id, action.digest)).status).toBe("outcome_unknown");
      expect(outcome(f, action).fact).toMatchObject({ status: "outcome_unknown", error: "outcome-unknown" });
      expect(outcome(f, action).fact).not.toHaveProperty("result");
      const context = await f.nextContext(action.sessionId);
      expect(context).toContain("outcome_unknown");
      expect(context).not.toContain("private-provider-error");
      await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("action-outcome_unknown");
      expect(f.invocations).toEqual([true]);
    } finally { await f.service.close(); }
  });

  test("expiry is a terminal fact without an invocation, and duplicate confirmation cannot append it", async () => {
    const f = fixture();
    try {
      const action = await f.propose();
      const clock = spyOn(Date, "now").mockReturnValue(action.expiresAt + 1);
      try {
        await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("action-expired");
      } finally { clock.mockRestore(); }
      const first = JSON.stringify(outcome(f, action).message);
      expect(outcome(f, action).fact).toMatchObject({ status: "expired", error: "action-expired" });
      await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("action-expired");
      expect(JSON.stringify(outcome(f, action).message)).toBe(first);
      expect(f.invocations).toEqual([]);
    } finally { await f.service.close(); }
  });

  test("double-confirm admits one simulated invocation and one outcome", async () => {
    const f = fixture();
    try {
      const action = await f.propose();
      let release!: () => void;
      let entered!: () => void;
      const barrier = new Promise<void>(resolve => { release = resolve; });
      const started = new Promise<void>(resolve => { entered = resolve; });
      f.tools.invoke = async () => {
        f.invocations.push(true);
        entered();
        await barrier;
        return { receipt: "once" };
      };
      const first = f.service.confirmAction(identity, action.id, action.digest);
      await Promise.race([started, first.then(() => { throw new Error("invocation-not-entered"); })]);
      try {
        await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("action-executing");
      } finally { release(); }
      expect((await first).status).toBe("completed");
      expect(outcome(f, action).fact.result).toEqual({ receipt: "once" });
      expect(f.invocations).toEqual([true]);
    } finally { await f.service.close(); }
  });

  test("only the existing safe JSON receipt is projected, including configured-key and sensitive-field redaction", async () => {
    const f = fixture({ secrets: () => ["fixture-secret"] });
    try {
      const action = await f.propose();
      f.tools.invoke = async () => ({
        receipt: "safe", password: "fixture-secret", echoed: "fixture-secret", "fixture-secret": "omit",
      });
      expect((await f.service.confirmAction(identity, action.id, action.digest)).status).toBe("completed");
      expect(outcome(f, action).fact.result).toEqual({
        receipt: "safe", password: "[redacted]", echoed: "[redacted]",
      });
      const context = await f.nextContext(action.sessionId);
      expect(context).not.toContain("fixture-secret");
      expect(context).not.toContain("private-prepared-input");
    } finally { await f.service.close(); }
  });

  test("mandatory outcome metadata does not consume the optional receipt budget", async () => {
    const f = fixture({ limits: { maxOutputBytes: 2048 } });
    try {
      const action = await f.propose();
      f.tools.invoke = async () => ({ receipt: "x".repeat(1900) });
      expect((await f.service.confirmAction(identity, action.id, action.digest)).status).toBe("completed");
      expect(outcome(f, action).fact.result.receipt).toHaveLength(1900);
    } finally { await f.service.close(); }
  });

  test("small output budgets cannot strand cancellation or interrupted recovery, even with a large legacy tool label", async () => {
    const f = fixture();
    const cancelled = await f.propose();
    const interrupted = await f.propose();
    await f.service.close();
    f.store.transaction(tx => {
      const action = tx.get("actions", interrupted.id)!;
      const call = tx.get("calls", action.callId)!;
      const prepared = { ...action.prepared, definition: { ...action.prepared.definition, name: "x".repeat(50_000) } };
      tx.put("actions", { ...action, prepared, status: "executing" });
      tx.put("calls", { ...call, prepared, status: "executing" });
      tx.put("runs", { ...tx.get("runs", action.runId)!, status: "running" });
    });
    const small = createAgentService({ ...f.options, limits: { maxOutputBytes: 256 } }, { streamFn: f.core.streamSimple });
    try {
      expect((await small.cancelAction(identity, cancelled.id)).status).toBe("cancelled");
      await small.recoverInterrupted();
      expect(f.store.transaction(tx => tx.get("runs", interrupted.runId)?.status)).toBe("interrupted");
      expect(f.store.transaction(tx => tx.get("actions", interrupted.id)?.status)).toBe("outcome_unknown");
      expect(outcome(f, cancelled).fact.status).toBe("cancelled");
      expect(outcome(f, interrupted).fact.toolName).toBe("[omitted: oversized label]");
      expect(outcome(f, interrupted).fact.status).toBe("outcome_unknown");
      expect(f.invocations).toEqual([]);
    } finally { await small.close(); }
  });

  test("oversized receipts cannot persist a false completed action, and do not re-invoke", async () => {
    const f = fixture({ limits: { maxOutputBytes: 2048 } });
    try {
      const action = await f.propose();
      f.tools.invoke = async () => {
        f.invocations.push(true);
        return { receipt: "x".repeat(2100) };
      };
      expect((await f.service.confirmAction(identity, action.id, action.digest)).status).toBe("outcome_unknown");
      const call = f.store.transaction(tx => tx.get("calls", action.callId)!);
      expect(call.status).toBe("outcome_unknown");
      expect(call.result).toBeUndefined();
      expect(outcome(f, action).fact).toMatchObject({ status: "outcome_unknown" });
      expect(outcome(f, action).fact).not.toHaveProperty("result");
      await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("action-outcome_unknown");
      expect(f.invocations).toEqual([true]);
    } finally { await f.service.close(); }
  });

  test("an outcome-message write failure atomically rolls completion back to unknown, without repeating the tool", async () => {
    const backing = createMemoryStore();
    let rejectCompletedFact = true;
    const store: AgentStore = {
      transaction: work => backing.transaction(tx => work({
        ...tx,
        put(table, record) {
          if (table === "messages" && rejectCompletedFact && record.id.startsWith("action-outcome:")) {
            rejectCompletedFact = false;
            throw new Error("offline-storage-fault");
          }
          tx.put(table, record);
        },
      })),
      close: () => backing.close(),
    };
    const f = fixture({ store });
    try {
      const action = await f.propose();
      expect((await f.service.confirmAction(identity, action.id, action.digest)).status).toBe("outcome_unknown");
      expect(f.store.transaction(tx => tx.get("calls", action.callId)?.status)).toBe("outcome_unknown");
      expect(outcome(f, action).fact.status).toBe("outcome_unknown");
      expect(outcome(f, action).fact).not.toHaveProperty("result");
      expect(f.invocations).toEqual([true]);
    } finally { await f.service.close(); }
  });
});

describe("legacy terminal action backfill", () => {
  test("backfill is scoped, bounded, deterministic, and never overwrites an existing fact", async () => {
    const f = fixture();
    try {
      const action = await f.propose();
      const completed: PendingAction = { ...action, status: "completed", result: { receipt: "legacy" } };
      f.store.transaction(tx => {
        tx.put("actions", completed);
        ensureActionOutcomes(tx, action.sessionId, [], 4096, 65536, () => false);
        expect(tx.get("messages", `action-outcome:${action.id}`)).toBeUndefined();
        ensureActionOutcomes(tx, "different-session", [], 4096, 65536, () => true);
        expect(tx.get("messages", `action-outcome:${action.id}`)).toBeUndefined();
        ensureActionOutcomes(tx, action.sessionId, [], 4096, 65536,
          candidate => sameIdentity(candidate.identity, identity));
      });
      const original = JSON.stringify(outcome(f, action).message);
      f.store.transaction(tx => {
        ensureActionOutcomes(tx, action.sessionId, [], 4096, 65536, () => true);
        persistActionOutcome(tx, { ...completed, result: { receipt: "must-not-replace" } }, [], 4096);
        persistActionOutcome(tx, action, [], 4096);
      });
      expect(JSON.stringify(outcome(f, action).message)).toBe(original);
      expect(outcome(f, action).fact.result).toEqual({ receipt: "legacy" });
      expect(f.invocations).toEqual([]);
      expect(() => f.store.transaction(tx => ensureActionOutcomes({
        ...tx,
        list() { throw new Error("must-not-materialize-over-budget-actions"); },
      }, action.sessionId, [], 4096, 1, () => true))).toThrow("snapshot-budget");
    } finally { await f.service.close(); }
  });
});

const childProgram = `
import { Database } from "bun:sqlite";
import { createFauxCore, fauxAssistantMessage, fauxToolCall } from "@earendil-works/pi-ai";
import { createAgentService } from ${JSON.stringify(new URL("../src/runtime.ts", import.meta.url).href)};
import { createSqliteStore, migrateAgentDatabase } from ${JSON.stringify(new URL("../src/sqlite.ts", import.meta.url).href)};
const identity = ${JSON.stringify(identity)};
const definition = ${JSON.stringify(write)};
const db = new Database(process.env.OUTCOME_DB);
migrateAgentDatabase(db);
const store = createSqliteStore(db, { owned: true });
const core = createFauxCore({models: [{id: "offline"}], tokenSize: {min: 1, max: 1}});
const tools = {
  async catalog() { return [definition]; },
  async prepare(name, input) { return {definition, input, precondition: "v1"}; },
  async invoke() {
    if (process.env.OUTCOME_MODE === "interrupted") process.exit(0);
    return {receipt: "child-process-receipt"};
  },
};
const service = createAgentService({
  store, tools, identity: async context => context, model: core.getModel(),
  profiles: [{id: "default", instructions: "Offline tools only.", tools: ["update"]}],
}, {streamFn: core.streamSimple});
if (process.env.OUTCOME_PHASE === "produce") {
  core.setResponses([fauxAssistantMessage(fauxToolCall("update", {id: "private-prepared-input"})),
    fauxAssistantMessage("Awaiting approval.")]);
  const session = await service.createSession(identity, "default");
  await (await service.startRun(identity, session.id, "Update")).done;
  const action = (await service.readSession(identity, session.id)).actions[0];
  console.log(JSON.stringify({sessionId: session.id, actionId: action.id}));
  await service.confirmAction(identity, action.id, action.digest);
} else {
  await service.recoverInterrupted();
  await service.recoverInterrupted();
  let context = "";
  core.setResponses([request => { context = JSON.stringify(request.messages); return fauxAssistantMessage("Observed."); }]);
  await (await service.startRun(identity, process.env.OUTCOME_SESSION, "What happened?")).done;
  const snapshot = await service.readSession(identity, process.env.OUTCOME_SESSION);
  const facts = snapshot.messages.filter(message => message.id.startsWith("action-outcome:"));
  console.log(JSON.stringify({context, facts, status: snapshot.actions[0].status}));
}
await service.close();
store.close();
`;

async function child(env: Record<string, string>) {
  const process = Bun.spawn([Bun.which("bun")!, "--eval", childProgram], {
    env: { ...Bun.env, ...env }, stdout: "pipe", stderr: "pipe",
  });
  const [stdout, stderr, code] = await Promise.all([
    new Response(process.stdout).text(), new Response(process.stderr).text(), process.exited,
  ]);
  expect(stderr).toBe("");
  expect(code).toBe(0);
  return JSON.parse(stdout.trim());
}

describe("independent local process restart with temporary SQLite", () => {
  for (const mode of ["completed", "interrupted"] as const) {
    test(`${mode} outcome survives a fresh executor and appears in real Pi context`, async () => {
      const dir = mkdtempSync(join(tmpdir(), "agent-outcomes-"));
      try {
        const env = { OUTCOME_DB: join(dir, "agent.sqlite"), OUTCOME_MODE: mode };
        const ids = await child({ ...env, OUTCOME_PHASE: "produce" });
        const restored = await child({ ...env, OUTCOME_PHASE: "consume", OUTCOME_SESSION: ids.sessionId });
        const status = mode === "completed" ? "completed" : "outcome_unknown";
        expect(restored.status).toBe(status);
        expect(restored.facts).toHaveLength(1);
        expect(restored.facts[0].id).toBe(`action-outcome:${ids.actionId}`);
        expect(restored.context).toContain(ids.actionId);
        expect(restored.context).toContain(status);
        expect(restored.context).not.toContain("private-prepared-input");
        expect(restored.context).not.toContain('"toolCall"');
        expect(restored.context.includes("child-process-receipt")).toBe(mode === "completed");
        const fact = JSON.parse(restored.facts[0].value.content);
        expect(fact.status).toBe(status);
        if (mode === "interrupted") expect(fact).not.toHaveProperty("result");
      } finally { rmSync(dir, { recursive: true, force: true }); }
    });
  }
});
