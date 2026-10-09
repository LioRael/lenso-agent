import { describe, expect, test } from "bun:test";
import {
  createFauxCore, fauxAssistantMessage, fauxText, fauxThinking, fauxToolCall,
  createAssistantMessageEventStream,
} from "@earendil-works/pi-ai";
import { createAgentService } from "../src/runtime";
import { createMemoryStore } from "../src/store";
import { RunEvents, SafeText } from "../src/events";
import type { AgentEvent, AgentOptions, AgentStore, Identity, ToolDefinition, ToolSource } from "../src/types";
import type { PiExtensions } from "../src/pi";

const identity: Identity = { subject: "alice", scope: "tenant-a", application: "test", target: "local" };
const read: ToolDefinition = {
  name: "lookup", description: "Read an item", inputSchema: { type: "object", properties: { id: { type: "string" } }, required: ["id"], additionalProperties: false },
  target: { application: "test", instance: "local", plugin: "items", operation: "lookup" },
  effect: "read", confirmation: false,
};
const write: ToolDefinition = { ...read, name: "update", effect: "write", confirmation: true };
function deferred<T = void>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
function fixture(overrides: Partial<AgentOptions<Identity>> = {}, pi: PiExtensions = {}) {
  const core = createFauxCore({ models: [{ id: "offline" }], tokenSize: { min: 1, max: 1 } });
  const store = overrides.store ?? createMemoryStore();
  const invoked: { name: string; confirmed: boolean; signal: AbortSignal }[] = [];
  const tools: ToolSource<Identity> = {
    async catalog() { return [read, write]; },
    async prepare(name, input) {
      const definition = [read, write].find(item => item.name === name);
      if (!definition) throw new Error("not open");
      return { definition, input, precondition: "v1" };
    },
    async invoke(prepared, _context, _identity, signal, confirmed) {
      invoked.push({ name: prepared.definition.name, confirmed, signal });
      return { item: "found" };
    },
  };
  const options: AgentOptions<Identity> = {
    store, identity: async context => context, tools,
    profiles: [{ id: "default", instructions: "Use only listed business tools.", tools: ["lookup", "update"] }],
    model: core.getModel(), ...overrides,
  };
  return { core, store, tools, invoked, options, service: createAgentService(options, { streamFn: core.streamSimple, ...pi }) };
}
async function collect(events: AsyncIterable<AgentEvent>) {
  const result: AgentEvent[] = [];
  for await (const event of events) result.push(event);
  return result;
}
const callRead = () => fauxAssistantMessage(fauxToolCall("lookup", { id: "1" }));
const callWrite = () => fauxAssistantMessage(fauxToolCall("update", { id: "1" }));

describe("real Pi execution", () => {
  test("read tools traverse durable gateway and continue with real tool results", async () => {
    const f = fixture();
    f.core.setResponses([callRead(), context => {
      const result = context.messages.find(message => message.role === "toolResult");
      expect(result?.role === "toolResult" && result.content).toEqual([{ type: "text", text: '{"item":"found"}' }]);
      return fauxAssistantMessage("Found.");
    }]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Lookup 1");
    const events = collect(handle.events);
    expect((await handle.done).status).toBe("completed");
    const saved = await f.service.readSession(identity, session.id);
    expect(f.invoked.map(item => item.name)).toEqual(["lookup"]);
    expect(saved.calls[0]?.status).toBe("completed");
    expect(saved.messages.map(item => item.value.role)).toEqual(["user", "assistant", "toolResult", "assistant"]);
    expect(handle.run.status).toBe("running");
    expect((await events).map(event => event.kind)).toContain("tool_result");
    expect((await f.service.getRun(identity, handle.run.id)).usage?.modelTurns).toBe(2);
    await f.service.close();
  });

  test("model cannot open a catalog tool excluded by profile", async () => {
    const f = fixture({ profiles: [{ id: "default", instructions: "", tools: ["lookup"] }] });
    f.core.setResponses([callWrite(), fauxAssistantMessage("Not available.")]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Update 1");
    await handle.done;
    expect(f.invoked).toEqual([]);
    expect((await f.service.readSession(identity, session.id)).actions).toEqual([]);
    await f.service.close();
  });

  test("write proposal returns without invoking; only matching authenticated human confirmation executes", async () => {
    const f = fixture();
    f.core.setResponses([callWrite(), fauxAssistantMessage("Awaiting approval.")]);
    const session = await f.service.createSession(identity, "default");
    await (await f.service.startRun(identity, session.id, "Update 1")).done;
    const action = (await f.service.readSession(identity, session.id)).actions[0]!;
    expect(action.status).toBe("pending");
    expect(f.invoked).toEqual([]);
    await expect(f.service.confirmAction({ ...identity, scope: "other" }, action.id, action.digest)).rejects.toThrow("not-found");
    await expect(f.service.confirmAction(identity, action.id, "wrong")).rejects.toThrow("confirmation-mismatch");
    expect((await f.service.confirmAction(identity, action.id, action.digest)).status).toBe("completed");
    expect(f.invoked.map(item => item.confirmed)).toEqual([true]);
    await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("action-completed");
    await f.service.close();
  });

  test("awaited final messages replay safely on the next run without raw call JSON", async () => {
    const f = fixture();
    f.core.setResponses([callRead(), fauxAssistantMessage("First answer"), context => {
      const json = JSON.stringify(context.messages);
      expect(json).toContain("First answer");
      expect(json).toContain("found");
      expect(json).not.toContain('"arguments"');
      expect(context.messages.filter(message => message.role === "assistant").every(message => message.content.length > 0)).toBe(true);
      return fauxAssistantMessage("Second answer");
    }]);
    const session = await f.service.createSession(identity, "default");
    await (await f.service.startRun(identity, session.id, "First")).done;
    expect((await (await f.service.startRun(identity, session.id, "Second")).done).status).toBe("completed");
    await f.service.close();
  });
});

describe("admission, identity, and draining", () => {
  test("atomic admission precedes blocked context loading across services sharing a store", async () => {
    const entered = deferred();
    const release = deferred();
    const f = fixture({ limits: { maxConcurrentRuns: 1 } });
    f.options.tools.catalog = async () => { entered.resolve(); await release.promise; return [read]; };
    const other = createAgentService(f.options, { streamFn: f.core.streamSimple });
    const session = await f.service.createSession(identity, "default");
    const second = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "First");
    await entered.promise;
    expect((await f.service.readSession(identity, session.id)).messages.map(message => message.value))
      .toEqual([{ role: "user", content: "First", timestamp: handle.run.createdAt }]);
    await expect(other.startRun(identity, session.id, "Same session")).rejects.toThrow("session-busy");
    await expect(other.startRun(identity, second.id, "Other session")).rejects.toThrow("concurrency-budget");
    await expect(f.service.recoverInterrupted()).rejects.toThrow("executor-active");
    f.core.setResponses([fauxAssistantMessage("Done")]);
    release.resolve();
    expect((await handle.done).status).toBe("completed");
    await f.service.close();
    await other.close();
  });

  test("all four identity dimensions protect sessions, runs, and event attachment", async () => {
    const f = fixture();
    f.core.setResponses([fauxAssistantMessage("Done")]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Hello");
    await handle.done;
    for (const key of ["subject", "scope", "application", "target"] as const) {
      const wrong = { ...identity, [key]: "different" };
      await expect(f.service.readSession(wrong, session.id)).rejects.toThrow("not-found");
      await expect(f.service.readMessages(wrong, session.id)).rejects.toThrow("not-found");
      await expect(f.service.getRun(wrong, handle.run.id)).rejects.toThrow("not-found");
      await expect(f.service.events(wrong, handle.run.id)).rejects.toThrow("not-found");
      await expect(f.service.watchRun(wrong, handle.run.id)).rejects.toThrow("not-found");
      await expect(f.service.cancelRun(wrong, handle.run.id)).rejects.toThrow("not-found");
      await expect(f.service.startRun(wrong, session.id, "Hello")).rejects.toThrow("not-found");
    }
    await f.service.close();
  });

  test("UI message pages hold an insertion watermark and never replace complete model history", async () => {
    const f = fixture();
    f.core.setResponses([fauxAssistantMessage("First answer"), context => {
      expect(JSON.stringify(context.messages)).toContain("First answer");
      expect(JSON.stringify(context.messages)).toContain("First question");
      return fauxAssistantMessage("Second answer");
    }]);
    const session = await f.service.createSession(identity, "default");
    await (await f.service.startRun(identity, session.id, "First question")).done;
    const first = await f.service.readMessages(identity, session.id, { limit: 1 });
    expect(first.items).toHaveLength(1);
    expect(first.hasMore).toBe(true);
    await (await f.service.startRun(identity, session.id, "Second question")).done;
    const rest = await f.service.readMessages(identity, session.id, {
      after: first.cursor, through: first.through, limit: 100,
    });
    expect(rest.items.map(item => item.value.role)).toEqual(["assistant"]);
    expect(rest.hasMore).toBe(false);
    expect((await f.service.readMessages(identity, session.id)).items).toHaveLength(4);
    await f.service.close();
  });

  test("message pages expose scoped record IDs, never global insertion counts", async () => {
    const f = fixture();
    const session = await f.service.createSession(identity, "default");
    const foreignOwner = { ...identity, scope: "foreign-scope" };
    const foreign = await f.service.createSession(foreignOwner, "default");
    const insert = (id: string, sessionId: string) => f.store.transaction(tx => tx.put("messages", {
      id, sessionId, runId: "old", createdAt: 1,
      value: { role: "user", content: id, timestamp: 1 },
    }));
    insert("first", session.id);
    insert("last", session.id);
    insert("foreign", foreign.id);
    const page = await f.service.readMessages(identity, session.id, { limit: 1 });
    expect(page.cursor).toBe("first");
    expect(page.through).toBe("last");
    insert("foreign-later", foreign.id);
    expect((await f.service.readMessages(identity, session.id, { limit: 1 })).through).toBe(page.through);
    for (const id of ["foreign", "absent"]) {
      await expect(f.service.readMessages(identity, session.id, { after: id })).rejects.toThrow("invalid-input");
      await expect(f.service.readMessages(identity, session.id, { through: id })).rejects.toThrow("invalid-input");
    }
    await expect(f.service.readMessages(foreignOwner, session.id, { after: page.cursor })).rejects.toThrow("not-found");
    await f.service.close();
  });

  test("watchRun joins a running snapshot to terminal live delivery and detaches without cancelling", async () => {
    const entered = deferred();
    const release = deferred();
    const f = fixture({}, { hooks: [{
      async beforeRun() { entered.resolve(); await release.promise; },
    }] });
    f.core.setResponses([fauxAssistantMessage("Authoritative final")]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Question");
    await entered.promise;
    const detached = await f.service.watchRun(identity, handle.run.id);
    const watch = await f.service.watchRun(identity, handle.run.id);
    expect(watch.run.status).toBe("running");
    expect(watch.sequence).toBeGreaterThan(0);
    await detached.close();
    expect((await f.service.getRun(identity, handle.run.id)).status).toBe("running");
    const events = (async () => {
      const result: AgentEvent[] = [];
      try { for await (const event of watch.events) result.push(event); }
      catch (error) { expect((error as Error).message).toContain("resnapshot-required"); }
      return result;
    })();
    release.resolve();
    expect((await handle.done).status).toBe("completed");
    const live = await events;
    expect(live.map(event => event.kind)).toContain("run_completed");
    expect(live.every(event => event.sequence > watch.sequence)).toBe(true);
    const settled = await f.service.watchRun(identity, handle.run.id);
    expect(settled.run.status).toBe("completed");
    expect(JSON.stringify(settled.snapshot.messages)).toContain("Authoritative final");
    expect(await collect(settled.events)).toEqual([]);
    await watch.close();
    await settled.close();
    await f.service.close();
  });

  test("oversized snapshots and histories fail before materializing session records", async () => {
    const backing = createMemoryStore();
    let listCalls = 0;
    const store: AgentStore = {
      transaction: work => backing.transaction(tx => work({
        ...tx,
        list(table, sessionId) {
          if (table === "messages") listCalls++;
          return tx.list(table, sessionId);
        },
      })),
      close: () => backing.close(),
    };
    const f = fixture({ store, limits: { maxSnapshotBytes: 512, maxHistoryBytes: 512 } });
    const session = await f.service.createSession(identity, "default");
    backing.transaction(tx => tx.put("messages", {
      id: "large-message", sessionId: session.id, runId: "old-run", createdAt: 0,
      value: { role: "user", content: "x".repeat(4096), timestamp: 0 },
    }));
    await expect(f.service.readSession(identity, session.id)).rejects.toThrow("snapshot-budget");
    await expect(f.service.startRun(identity, session.id, "New question")).rejects.toThrow("history-budget");
    expect(listCalls).toBe(0);
    expect(backing.transaction(tx => tx.count("runs", { sessionId: session.id }))).toBe(0);
    await f.service.close();
  });

  test("malformed host identity cannot create unowned sessions", async () => {
    for (const invalid of [
      {}, { ...identity, subject: "" }, { ...identity, scope: " " }, { ...identity, target: "x".repeat(513) },
    ]) {
      const f = fixture({ identity: async () => invalid as Identity });
      await expect(f.service.createSession(identity, "default")).rejects.toThrow("invalid-identity");
      expect(f.store.transaction(tx => tx.list("sessions"))).toEqual([]);
      await f.service.close();
    }
  });

  test("cancel and close abort but do not settle while a real noncooperative tool still executes", async () => {
    const entered = deferred<AbortSignal>();
    const release = deferred();
    const f = fixture();
    f.options.tools.invoke = async (_prepared, _context, _identity, signal) => {
      entered.resolve(signal);
      await release.promise;
      return { finished: true };
    };
    f.core.setResponses([callRead()]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Read");
    const signal = await entered.promise;
    let settled = false;
    const cancelled = f.service.cancelRun(identity, handle.run.id).then(run => { settled = true; return run; });
    await Bun.sleep(0);
    expect(signal.aborted).toBe(true);
    expect(settled).toBe(false);
    expect((await f.service.getRun(identity, handle.run.id)).status).toBe("running");
    await expect(f.service.startRun(identity, session.id, "Interleave")).rejects.toThrow("session-busy");
    let closed = false;
    const closing = f.service.close().then(() => { closed = true; });
    await Bun.sleep(0);
    expect(closed).toBe(false);
    release.resolve();
    expect((await cancelled).status).toBe("cancelled");
    await closing;
    expect(f.store.transaction(tx => tx.list("calls", session.id)[0]?.status)).toBe("completed");
    expect(f.store.transaction(tx => tx.list("sessions").length)).toBe(1);
  });

  test("close drains human confirmations and prevents run/confirmation interleaving", async () => {
    const entered = deferred<AbortSignal>();
    const release = deferred();
    const f = fixture({ limits: { maxConcurrentRuns: 1 } });
    f.core.setResponses([callWrite(), fauxAssistantMessage("Approve")]);
    const session = await f.service.createSession(identity, "default");
    const other = await f.service.createSession(identity, "default");
    await (await f.service.startRun(identity, session.id, "Write")).done;
    const action = (await f.service.readSession(identity, session.id)).actions[0]!;
    f.options.tools.invoke = async (_prepared, _context, _identity, signal) => {
      entered.resolve(signal); await release.promise; return { ok: true };
    };
    const confirmation = f.service.confirmAction(identity, action.id, action.digest);
    const signal = await entered.promise;
    await expect(f.service.startRun(identity, session.id, "Interleave")).rejects.toThrow("session-busy");
    await expect(f.service.startRun(identity, other.id, "Concurrent")).rejects.toThrow("concurrency-budget");
    let closed = false;
    const closing = f.service.close().then(() => { closed = true; });
    await Bun.sleep(0);
    expect(signal.aborted).toBe(true);
    expect(closed).toBe(false);
    release.resolve();
    expect((await confirmation).status).toBe("completed");
    await closing;
  });
});

describe("safe bounded output and events", () => {
  test("provider credentials are redacted at durable tool receipts, not just the live transcript", async () => {
    const key = "dummy-provider-credential";
    const f = fixture({}, { getApiKey: async () => key });
    f.options.tools.invoke = async () => ({ item: key, [key]: "ordinary metadata" });
    f.core.setResponses([callRead(), fauxAssistantMessage("Done")]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Read");
    expect((await handle.done).status).toBe("completed");
    const snapshot = await f.service.readSession(identity, session.id);
    expect(snapshot.calls[0].result).toEqual({ item: "[redacted]" });
    expect(JSON.stringify(f.store.transaction(tx => tx.list("calls")))).not.toContain(key);
    expect(JSON.stringify(snapshot)).not.toContain(key);
    expect(JSON.stringify(await collect(handle.events))).not.toContain(key);
    await f.service.close();
  });

  test("a provider consumer error aborts and drains its producer's terminal result", async () => {
    const entered = deferred();
    const release = deferred();
    let providerSignal!: AbortSignal;
    const f = fixture({}, { streamFn: (_model, _context, options) => {
      providerSignal = options!.signal!;
      const stream = createAssistantMessageEventStream();
      const partial = fauxAssistantMessage("Safe");
      stream.push({ type: "start", partial });
      const original = stream[Symbol.asyncIterator].bind(stream);
      stream[Symbol.asyncIterator] = async function* () {
        const first = await original().next();
        if (!first.done) yield first.value;
        throw new Error("private provider iterator error");
      };
      void (async () => {
        entered.resolve();
        await release.promise;
        stream.end(partial);
      })();
      return stream;
    } });
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Hello");
    let settled = false;
    void handle.done.then(() => { settled = true; });
    await entered.promise;
    await Bun.sleep(0);
    expect(providerSignal.aborted).toBe(true);
    expect(settled).toBe(false);
    expect((await f.service.getRun(identity, handle.run.id)).status).toBe("running");
    let closed = false;
    const closing = f.service.close().then(() => { closed = true; });
    await Bun.sleep(0);
    expect(closed).toBe(false);
    release.resolve();
    await handle.done;
    await closing;
    expect(JSON.stringify(await collect(handle.events))).not.toContain("private provider");
  });

  test("event copies honor a configured output ceiling above one MiB", () => {
    const events = new RunEvents("session", "run", 2, 2 * 1024 * 1024);
    const stream = events.subscribe()[Symbol.asyncIterator]();
    const text = "x".repeat(1_100_000);
    expect(() => events.emit("text_delta", { data: { text } })).not.toThrow();
    return stream.next().then(async next => {
      expect(next.done).toBe(false);
      expect((next.value!.data as { text: string }).text.length).toBe(text.length);
      await stream.return?.();
    });
  });

  test("Pi customization gets detached transcripts and provider options, never executable gateway tools", async () => {
    let f!: ReturnType<typeof fixture>;
    f = fixture({}, {
      transformContext: async transcript => {
        expect(JSON.stringify(transcript)).not.toContain('"execute"');
        const messages = structuredClone(transcript);
        transcript.splice(0, transcript.length);
        return messages;
      },
      streamFn: (model, context, options) => {
        expect(Object.keys(options ?? {}).sort()).toEqual(["apiKey", "maxTokens", "reasoning", "signal"]);
        return f.core.streamSimple(model, context, options);
      },
    });
    f.core.setResponses([callRead(), fauxAssistantMessage("Done")]);
    const session = await f.service.createSession(identity, "default");
    expect((await (await f.service.startRun(identity, session.id, "Hello")).done).status).toBe("completed");
    expect(f.invoked.length).toBe(1);
    await f.service.close();
  });

  test("split provider keys are redacted in genuine text increments and stored finals; reasoning is never exposed", async () => {
    const key = "credential-split-across-many-chunks";
    const f = fixture({ secrets: () => ["host-secret"] }, { getApiKey: () => key });
    f.core.setResponses([fauxAssistantMessage([fauxThinking("private reasoning"), fauxText(`Before ${key}, host-secret after.`)])]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Hello");
    const stream = collect(handle.events);
    await handle.done;
    const events = await stream;
    const text = events.filter(event => event.kind === "text_delta").map(event => (event.data as { text: string }).text).join("");
    expect(text).toBe("Before [redacted], [redacted] after.");
    expect(events.filter(event => event.kind === "text_delta").length).toBeGreaterThan(1);
    const json = JSON.stringify(await f.service.readSession(identity, session.id));
    expect(json).not.toContain(key);
    expect(json).not.toContain("host-secret");
    expect(json).not.toContain("private reasoning");
    expect(json).not.toContain('"type":"thinking"');
    await f.service.close();
  });

  test("provider errors expose only safe status and unknown usage remains null", async () => {
    const f = fixture({}, { streamFn: () => {
      const stream = createAssistantMessageEventStream();
      const error = fauxAssistantMessage([], { stopReason: "error", errorMessage: "unsafe provider request payload" });
      stream.push({ type: "error", reason: "error", error });
      return stream;
    } });
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Hello");
    const run = await handle.done;
    expect(run.error).toBe("provider-failed");
    expect(run.usage?.inputTokens).toBeNull();
    expect(run.usage?.outputTokens).toBeNull();
    expect(JSON.stringify(await f.service.readSession(identity, session.id))).not.toContain("unsafe provider");
    expect(JSON.stringify(await collect(handle.events))).not.toContain("unsafe provider");
    await f.service.close();
  });

  test("safe prefix scanner does not cut through full matches or leak incomplete prefixes", () => {
    const scanner = new SafeText(() => ["abcde", "xyz", "xy"]);
    expect(scanner.push("safe ab")).toBe("safe ");
    expect(scanner.push("cde and x")).toBe("[redacted] and ");
    expect(scanner.push("yz end ab")).toBe("[redacted] end ");
    expect(scanner.push("", true)).toBe("ab");
  });

  test("aborted assistant output cannot persist an unfinished credential prefix", async () => {
    const key = "unfinished-provider-key-credential";
    const prefix = key.slice(0, -1);
    const f = fixture({}, {
      getApiKey: async () => key,
      streamFn: () => {
        const stream = createAssistantMessageEventStream();
        const partial = fauxAssistantMessage(prefix);
        stream.push({ type: "start", partial });
        stream.push({ type: "text_delta", contentIndex: 0, delta: prefix, partial });
        stream.push({ type: "error", reason: "aborted", error: { ...partial, stopReason: "aborted" } });
        return stream;
      },
    });
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Hello");
    expect((await handle.done).error).toBe("provider-aborted");
    expect(JSON.stringify(await f.service.readSession(identity, session.id))).not.toContain(prefix);
    expect(JSON.stringify(await collect(handle.events))).not.toContain(prefix);
    await f.service.close();
  });

  test("overflow detaches with safe error, detach does not cancel execution, and settled streams have no replay", async () => {
    const release = deferred();
    const entered = deferred();
    const f = fixture({ limits: { eventBuffer: 2 } });
    f.core.setResponses([async () => { entered.resolve(); await release.promise; return fauxAssistantMessage("Many chunks of normal output"); }]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Hello");
    await entered.promise;
    const detached = (await f.service.events(identity, handle.run.id))[Symbol.asyncIterator]();
    await detached.return?.();
    release.resolve();
    expect((await handle.done).status).toBe("completed");
    expect((await collect(handle.events)).map(event => event.kind)).toEqual(["safe_error"]);
    expect(await collect(await f.service.events(identity, handle.run.id))).toEqual([]);
    await f.service.close();
  });

  test("fan-out caps subscribers and never lets a consumer mutate another consumer's event", async () => {
    const events = new RunEvents("session", "run", 2);
    const first = events.subscribe()[Symbol.asyncIterator]();
    const second = events.subscribe()[Symbol.asyncIterator]();
    expect(() => events.subscribe()).toThrow("too-many-subscribers");
    events.emit("text_delta", { data: { text: "safe" } });
    const one = await first.next();
    (one.value.data as { text: string }).text = "changed";
    expect((await second.next()).value.data).toEqual({ text: "safe" });
    await first.return?.();
    events.subscribe();
    events.close();
  });
});

describe("budgets and offline maintenance", () => {
  test("human-confirmed execution has its own cooperative deadline and never retries an unknown effect", async () => {
    const aborted = deferred();
    const f = fixture({ limits: { timeoutMs: 20 } });
    f.core.setResponses([callWrite(), fauxAssistantMessage("Pending")]);
    const session = await f.service.createSession(identity, "default");
    await (await f.service.startRun(identity, session.id, "Update")).done;
    const action = (await f.service.readSession(identity, session.id)).actions[0];
    let calls = 0;
    f.options.tools.invoke = async (_prepared, _context, _identity, signal) => {
      calls++;
      if (!signal.aborted) await new Promise<void>(resolve => {
        signal.addEventListener("abort", () => { aborted.resolve(); resolve(); }, { once: true });
      });
      signal.throwIfAborted();
      return {};
    };
    const confirmation = await f.service.confirmAction(identity, action.id, action.digest);
    await aborted.promise;
    expect(confirmation.status).toBe("outcome_unknown");
    await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("action-outcome_unknown");
    expect(calls).toBe(1);
    await f.service.close();
  });

  test("an aggregate snapshot ceiling cannot turn a saved completion into a missing run receipt", async () => {
    const f = fixture({ limits: { maxSnapshotBytes: 300 } });
    f.core.setResponses([fauxAssistantMessage("Done")]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Hello");
    const events = collect(handle.events);
    expect((await handle.done).status).toBe("completed");
    expect((await f.service.getRun(identity, handle.run.id)).status).toBe("completed");
    await expect(f.service.readSession(identity, session.id)).rejects.toThrow("snapshot-budget");
    expect((await events).map(event => event.kind)).toContain("run_completed");
    expect(f.store.transaction(tx => tx.get("runs", handle.run.id)?.status)).toBe("completed");
    await f.service.close();
  });

  test("a missing final run receipt releases live resources but requires explicit offline classification", async () => {
    const backing = createMemoryStore();
    let rejectReceipt = true;
    const store: AgentStore = {
      transaction: work => backing.transaction(tx => work({
        ...tx,
        put(table, record) {
          if (rejectReceipt && table === "runs" && "status" in record && record.status !== "running")
            throw new Error("private storage failure");
          tx.put(table, record);
        },
      })),
      close: () => backing.close(),
    };
    const f = fixture({ store });
    f.core.setResponses([fauxAssistantMessage("Done")]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Accepted prompt");
    await expect(handle.done).rejects.toThrow("run-receipt-unavailable");
    expect((await collect(handle.events)).map(event => event.kind)).toContain("run_interrupted");
    expect((await f.service.getRun(identity, handle.run.id)).status).toBe("running");
    rejectReceipt = false;
    await f.service.recoverInterrupted();
    expect((await f.service.getRun(identity, handle.run.id)).status).toBe("interrupted");
    f.core.setResponses([fauxAssistantMessage("Another answer")]);
    expect((await (await f.service.startRun(identity, session.id, "New prompt")).done).status).toBe("completed");
    await f.service.close();
  });

  test("ordered before hooks fail closed; after hooks observe persisted terminal state and drain close", async () => {
    const order: string[] = [];
    const entered = deferred();
    const release = deferred();
    let f!: ReturnType<typeof fixture>;
    f = fixture({ limits: { maxConcurrentRuns: 1 } }, { hooks: [{
      beforeRun: async fact => {
        expect(Object.keys(fact).sort()).toEqual(["runId", "sessionId"]);
        order.push("before-1");
      },
      afterRun: async fact => {
        expect(f.store.transaction(tx => tx.get("runs", fact.runId)?.status)).toBe("completed");
        order.push("after-1");
        throw new Error("private hook error");
      },
    }, {
      beforeRun: async () => { order.push("before-2"); },
      afterRun: async (_fact, signal) => {
        order.push("after-2");
        entered.resolve();
        await release.promise;
        expect(signal.aborted).toBe(true);
      },
    }] });
    f.core.setResponses([fauxAssistantMessage("Done")]);
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Hello");
    const events = collect(handle.events);
    await entered.promise;
    const otherSession = await f.service.createSession(identity, "default");
    await expect(f.service.startRun(identity, otherSession.id, "Another run"))
      .rejects.toThrow("concurrency-budget");
    let closed = false;
    const closing = f.service.close().then(() => { closed = true; });
    await Bun.sleep(0);
    expect(closed).toBe(false);
    release.resolve();
    expect((await handle.done).status).toBe("completed");
    await closing;
    expect(order).toEqual(["before-1", "before-2", "after-1", "after-2"]);
    const errors = (await events).filter(event => event.kind === "safe_error");
    expect(errors.map(event => event.data)).toEqual([{ code: "after-run-hook-failed" }]);

    const blocked = fixture({}, { hooks: [{ beforeRun: async () => { throw new Error("private failure"); } }] });
    const blockedSession = await blocked.service.createSession(identity, "default");
    const run = await (await blocked.service.startRun(identity, blockedSession.id, "Hello")).done;
    expect(run.status).toBe("failed");
    expect(run.error).toBe("operation-failed");
    expect(blocked.core.state.callCount).toBe(0);
    expect(blocked.invoked).toEqual([]);
    expect((await blocked.service.readSession(identity, blockedSession.id)).messages.map(message => message.value.role))
      .toEqual(["user"]);
    await blocked.service.close();
  });

  test("model-turn and tool-call budgets stop real Pi scheduling before another effect", async () => {
    for (const limits of [{ maxModelTurns: 1 }, { maxToolCalls: 1 }]) {
      const f = fixture({ limits });
      f.core.setResponses([fauxAssistantMessage([fauxToolCall("lookup", { id: "1" }), fauxToolCall("lookup", { id: "2" })]), callRead()]);
      const session = await f.service.createSession(identity, "default");
      const run = await (await f.service.startRun(identity, session.id, "Read")).done;
      expect(run.status).toBe("failed");
      expect(run.error).toBe("maxModelTurns" in limits ? "model-turn-budget" : "tool-call-budget");
      expect(f.invoked.length).toBe("maxModelTurns" in limits ? 2 : 1);
      expect(f.core.state.callCount).toBe(1);
      await f.service.close();
    }
  });

  test("context/history/output/input byte budgets bound different admission stages", async () => {
    const input = fixture({ limits: { maxInputBytes: 4 } });
    const session = await input.service.createSession(identity, "default");
    await expect(input.service.startRun(identity, session.id, "12345")).rejects.toThrow("input-budget");
    expect(input.store.transaction(tx => tx.list("runs").length)).toBe(0);
    await input.service.close();
    for (const limits of [{ maxContextBytes: 1 }, { maxHistoryBytes: 1 }, { maxOutputBytes: 600 }]) {
      const f = fixture({ limits });
      f.core.setResponses([fauxAssistantMessage("x".repeat(1000))]);
      const session = await f.service.createSession(identity, "default");
      if ("maxHistoryBytes" in limits) f.store.transaction(tx => tx.put("messages", {
        id: "old", sessionId: session.id, runId: "old", createdAt: 1,
        value: { role: "user", content: "Earlier", timestamp: 1 },
      }));
      if ("maxHistoryBytes" in limits) {
        await expect(f.service.startRun(identity, session.id, "Hello")).rejects.toThrow("history-budget");
        expect(f.store.transaction(tx => tx.list("runs").length)).toBe(0);
      } else {
        const run = await (await f.service.startRun(identity, session.id, "Hello")).done;
        expect(run.status).toBe("failed");
        expect(run.error).toBe("maxContextBytes" in limits ? "context-budget" : "output-budget");
      }
      await f.service.close();
    }
  });

  test("transcript extension cannot inflate model context past the combined budget", async () => {
    const f = fixture({
      limits: { maxContextBytes: 4096, maxHistoryBytes: 4096, maxOutputBytes: 4096, maxInputBytes: 4096 },
    }, {
      transformContext: async transcript => [...transcript, { role: "user", content: "x".repeat(20_000), timestamp: 1 }],
    });
    const session = await f.service.createSession(identity, "default");
    const run = await (await f.service.startRun(identity, session.id, "Hello")).done;
    expect(run.error).toBe("history-budget");
    expect(f.core.state.callCount).toBe(0);
    await f.service.close();
  });

  test("duration abort covers protected catalog loading and waits for it to drain", async () => {
    const entered = deferred<AbortSignal>();
    const release = deferred();
    const f = fixture({ limits: { timeoutMs: 10 } });
    f.options.tools.catalog = async (_context, _identity, signal) => { entered.resolve(signal); await release.promise; return [read]; };
    const session = await f.service.createSession(identity, "default");
    const handle = await f.service.startRun(identity, session.id, "Hello");
    const signal = await entered.promise;
    await Bun.sleep(20);
    expect(signal.aborted).toBe(true);
    expect((await f.service.getRun(identity, handle.run.id)).status).toBe("running");
    release.resolve();
    expect((await handle.done).error).toBe("duration-budget");
    expect(f.core.state.callCount).toBe(0);
    await f.service.close();
  });

  test("invalid non-positive/fractional limits fail before creating a service", () => {
    for (const timeoutMs of [0, -1, 1.5, Infinity, NaN]) expect(() => fixture({ limits: { timeoutMs } })).toThrow("invalid-limits");
  });

  test("explicit offline recovery classifies durable intent without replaying effects", async () => {
    const f = fixture();
    f.core.setResponses([callWrite(), fauxAssistantMessage("Approve")]);
    const session = await f.service.createSession(identity, "default");
    const run = await (await f.service.startRun(identity, session.id, "Write")).done;
    f.store.transaction(tx => {
      tx.put("runs", { ...run, status: "running" });
      const call = tx.list("calls", session.id)[0]!;
      const action = tx.list("actions", session.id)[0]!;
      tx.put("calls", { ...call, status: "executing" });
      tx.put("actions", { ...action, status: "executing" });
    });
    await f.service.recoverInterrupted();
    const state = await f.service.readSession(identity, session.id);
    expect(state.runs[0]?.status).toBe("interrupted");
    expect(state.calls[0]?.status).toBe("outcome_unknown");
    expect(state.actions[0]?.status).toBe("outcome_unknown");
    expect(f.invoked).toEqual([]);
    await expect(f.service.confirmAction(identity, state.actions[0]!.id, state.actions[0]!.digest)).rejects.toThrow("action-outcome_unknown");
    await f.service.close();
  });
});
