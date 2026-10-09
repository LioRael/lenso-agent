import { describe, expect, test, spyOn } from "bun:test";
import { createFauxCore } from "@earendil-works/pi-ai";
import { createExecutionGateway } from "../src/gateway";
import { createMemoryStore } from "../src/store";
import { AgentError, jsonCopy } from "../src/safety";
import type {
  AgentOptions, Identity, PendingAction, PreparedTool, RunLimits, ToolSource,
} from "../src/types";

const limits: RunLimits = {
  maxConcurrentRuns: 2, maxModelTurns: 4, maxToolCalls: 4,
  maxInputBytes: 4096, maxOutputBytes: 4096, maxContextBytes: 4096,
  maxHistoryBytes: 16384, maxSnapshotBytes: 65536, timeoutMs: 1000, eventBuffer: 16, actionTtlMs: 60000,
};
const identity: Identity = {
  subject: "alice", scope: "personal", application: "notes-app", target: "notes-runtime",
};
interface Context { subject: string; allowed: boolean }

function fixture() {
  const store = createMemoryStore();
  const context: Context = { subject: "alice", allowed: true };
  let version = "1";
  let effects = 0;
  let failReceipt = false;
  let barrier: Promise<void> | undefined;
  let auditFailure = false;
  const session = { id: "session-1", identity, profile: "notes", createdAt: 1 };
  const run = { id: "run-1", sessionId: session.id, status: "running" as const, createdAt: 1 };
  const definition = {
    name: "notes_update", description: "Update a note",
    inputSchema: { type: "object" },
    target: { application: identity.application, instance: identity.target, plugin: "notes", operation: "update" },
    effect: "write" as const, confirmation: true,
  };
  const source: ToolSource<Context> = {
    async catalog() { return [definition]; },
    async prepare(name, input, ctx) {
      if (!ctx.allowed) throw new AgentError("forbidden-operation");
      if (name !== definition.name) throw new AgentError("tool-not-open");
      if (!input || typeof input !== "object" || typeof Reflect.get(input, "title") !== "string")
        throw new AgentError("invalid-input");
      return { definition: jsonCopy(definition), input: jsonCopy(input), precondition: version };
    },
    async invoke(_prepared, ctx, _identity, signal) {
      if (!ctx.allowed) throw new AgentError("forbidden-operation");
      signal.throwIfAborted();
      effects++;
      if (barrier) await barrier;
      if (failReceipt) throw new Error("secret-provider-error");
      return { updated: true, password: "business-secret", "business-secret": "ordinary" };
    },
  };
  const options: AgentOptions<Context> = {
    store, tools: source, identity: async (ctx) => ({ ...identity, subject: ctx.subject }),
    model: createFauxCore({ api: "offline", provider: "offline" }).getModel(),
    profiles: [{ id: "notes", instructions: "Notes", tools: ["notes_update"] }],
    secrets: () => ["business-secret"],
    audit: async () => { if (auditFailure) throw new Error("private audit error"); },
  };
  store.transaction((tx) => { tx.put("sessions", session); tx.put("runs", run); });
  const gateway = createExecutionGateway(options, limits);
  const signal = new AbortController().signal;
  async function propose(input: unknown = { title: "updated", expectedVersion: "1" }): Promise<PendingAction> {
    await gateway.propose(context, session, run, "model-call-1", "notes_update", input, signal, () => {});
    store.transaction((tx) => tx.put("runs", { ...run, status: "completed" }));
    return store.transaction((tx) => tx.list("actions")[0]);
  }
  return {
    store, context, gateway, options, session, run, signal, propose,
    effects: () => effects,
    setVersion: (value: string) => { version = value; },
    setBarrier: (value: Promise<void>) => { barrier = value; },
    failReceipt: () => { failReceipt = true; },
    failAudit: () => { auditFailure = true; },
  };
}

// These tests prove confirmation consumption independently of Pi/model scheduling;
// Manage/schema and real Auth are exercised by their integration fixtures.
describe("concrete confirmation boundary", () => {
  test("a model proposal persists a concrete action without invoking the write", async () => {
    const f = fixture();
    const action = await f.propose();
    expect(f.effects()).toBe(0);
    expect(action.prepared.input).toEqual({ title: "updated", expectedVersion: "1" });
    expect(action.identity).toEqual(identity);
    const result = await f.gateway.confirm(f.context, action.id, action.digest, f.signal);
    expect(result.status).toBe("completed");
    expect(result.result).toEqual({ updated: true, password: "[redacted]" });
    expect(f.effects()).toBe(1);
    await expect(f.gateway.confirm(f.context, action.id, action.digest, f.signal))
      .rejects.toMatchObject({ code: "action-completed" });
    expect(f.effects()).toBe(1);
  });

  test("double-click confirmation atomically admits only one business effect", async () => {
    const f = fixture();
    const action = await f.propose();
    let release!: () => void;
    f.setBarrier(new Promise<void>((resolve) => { release = resolve; }));
    const first = f.gateway.confirm(f.context, action.id, action.digest, f.signal);
    const second = f.gateway.confirm(f.context, action.id, action.digest, f.signal);
    release();
    const results = await Promise.allSettled([first, second]);
    expect(results.filter((item) => item.status === "fulfilled")).toHaveLength(1);
    expect(f.effects()).toBe(1);
  });

  test("a forged digest or a different authenticated owner cannot approve", async () => {
    const f = fixture();
    const action = await f.propose();
    await expect(f.gateway.confirm(f.context, action.id, "approved:true", f.signal))
      .rejects.toMatchObject({ code: "confirmation-mismatch" });
    await expect(f.gateway.confirm({ subject: "bob", allowed: true }, action.id, action.digest, f.signal))
      .rejects.toMatchObject({ code: "forbidden" });
    expect(f.effects()).toBe(0);
  });

  test("tampering with persisted parameters or call target invalidates the confirmation", async () => {
    for (const table of ["actions", "calls"] as const) {
      const f = fixture();
      const action = await f.propose();
      f.store.transaction((tx) => {
        if (table === "actions") {
          tx.put("actions", { ...action, prepared: { ...action.prepared, input: { title: "forged" } } });
        } else {
          const call = tx.get("calls", action.callId)!;
          tx.put("calls", {
            ...call, prepared: {
              ...call.prepared, definition: { ...call.prepared.definition,
                target: { ...call.prepared.definition.target, instance: "other-runtime" } },
            },
          });
        }
      });
      await expect(f.gateway.confirm(f.context, action.id, action.digest, f.signal))
        .rejects.toMatchObject({ code: "confirmation-mismatch" });
      expect(f.effects()).toBe(0);
    }
  });

  test("version changes and permission revocation after proposal prevent execution", async () => {
    const f = fixture();
    const action = await f.propose();
    f.setVersion("2");
    await expect(f.gateway.confirm(f.context, action.id, action.digest, f.signal))
      .rejects.toMatchObject({ code: "precondition-conflict" });
    f.setVersion("1");
    f.context.allowed = false;
    await expect(f.gateway.confirm(f.context, action.id, action.digest, f.signal))
      .rejects.toMatchObject({ code: "forbidden-operation" });
    expect(f.effects()).toBe(0);
  });

  test("confirmation digest also protects expiration and correlation metadata", async () => {
    for (const changed of [
      { expiresAt: Date.now() + 9_000_000 },
      { runId: "another-run" },
      { sessionId: "another-session" },
      { callId: "another-call" },
    ]) {
      const f = fixture();
      const action = await f.propose();
      f.store.transaction((tx) => {
        tx.put("actions", { ...action, ...changed });
        if (changed.sessionId) tx.put("sessions", { ...f.session, id: changed.sessionId });
      });
      await expect(f.gateway.confirm(f.context, action.id, action.digest, f.signal))
        .rejects.toMatchObject({ code: "confirmation-mismatch" });
      expect(f.effects()).toBe(0);
    }
  });

  test("expiry and explicit cancellation are durable terminal rejections", async () => {
    const expired = fixture();
    const action = await expired.propose();
    const clock = spyOn(Date, "now").mockReturnValue(action.expiresAt + 1);
    try {
      await expect(expired.gateway.confirm(expired.context, action.id, action.digest, expired.signal))
        .rejects.toMatchObject({ code: "action-expired" });
    } finally { clock.mockRestore(); }
    expect(expired.store.transaction((tx) => tx.get("actions", action.id)?.status)).toBe("expired");
    const cancelled = fixture();
    const pending = await cancelled.propose();
    expect((await cancelled.gateway.cancel(cancelled.context, pending.id)).status).toBe("cancelled");
    await expect(cancelled.gateway.confirm(cancelled.context, pending.id, pending.digest, cancelled.signal))
      .rejects.toMatchObject({ code: "action-cancelled" });
    expect(expired.effects() + cancelled.effects()).toBe(0);
  });

  test("a running turn holds the session until it really finishes", async () => {
    const f = fixture();
    const action = await f.propose();
    f.store.transaction((tx) => tx.put("runs", f.run));
    await expect(f.gateway.confirm(f.context, action.id, action.digest, f.signal))
      .rejects.toMatchObject({ code: "session-busy" });
    expect(f.effects()).toBe(0);
  });

  test("effect without a receipt becomes unknown and cannot be confirmed again", async () => {
    const f = fixture();
    const action = await f.propose();
    f.failReceipt();
    const result = await f.gateway.confirm(f.context, action.id, action.digest, f.signal);
    expect(result.status).toBe("outcome_unknown");
    expect(result.error).toBe("outcome-unknown");
    await expect(f.gateway.confirm(f.context, action.id, action.digest, f.signal))
      .rejects.toMatchObject({ code: "action-outcome_unknown" });
    expect(f.effects()).toBe(1);
    expect(JSON.stringify(f.store.transaction((tx) => tx.list("actions"))))
      .not.toContain("secret-provider-error");
  });

  test("lossy non-JSON tool results cannot be recorded as successful facts", async () => {
    for (const result of [{ total: Infinity }, { missing: undefined }]) {
      const f = fixture();
      const action = await f.propose();
      f.options.tools.invoke = async () => result;
      const confirmed = await f.gateway.confirm(f.context, action.id, action.digest, f.signal);
      expect(confirmed.status).toBe("outcome_unknown");
      expect(confirmed.result).toBeUndefined();
    }
  });

  test("audit admission failure leaves a consumed rejection without a business effect", async () => {
    const f = fixture();
    const action = await f.propose();
    f.failAudit();
    await expect(f.gateway.confirm(f.context, action.id, action.digest, f.signal))
      .rejects.toMatchObject({ code: "operation-failed" });
    expect(f.effects()).toBe(0);
    expect(f.store.transaction((tx) => tx.get("actions", action.id)?.status)).toBe("rejected");
  });

  test("unopened operations and credentials are rejected before durable proposal", async () => {
    const f = fixture();
    await expect(f.gateway.propose(f.context, f.session, f.run, "x", "shell",
      {}, f.signal, () => {})).rejects.toMatchObject({ code: "tool-not-open" });
    await expect(f.gateway.propose(f.context, f.session, f.run, "business-secret", "notes_update",
      { title: "fine" }, f.signal, () => {})).rejects.toMatchObject({ code: "sensitive-input" });
    await expect(f.propose({ title: "business-secret" }))
      .rejects.toMatchObject({ code: "sensitive-input" });
    await expect(f.propose({ title: "fine", authorization: "model-token" }))
      .rejects.toMatchObject({ code: "sensitive-input" });
    await expect(f.propose({ title: "fine", access_token: "model-token" }))
      .rejects.toMatchObject({ code: "sensitive-input" });
    await expect(f.propose({ title: "fine", "business-secret": "ordinary" }))
      .rejects.toMatchObject({ code: "sensitive-input" });
    await expect(f.propose({ title: "Bearer not-a-model-context-value" }))
      .rejects.toMatchObject({ code: "sensitive-input" });
    expect(f.store.transaction((tx) => tx.list("actions"))).toEqual([]);
    expect(f.store.transaction((tx) => tx.list("calls"))).toEqual([]);
  });
});
