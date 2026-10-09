import { describe, expect, test } from "bun:test";
import { createFauxCore, fauxAssistantMessage, fauxToolCall } from "@earendil-works/pi-ai/providers/faux";
import { createAgentFetchHandler } from "../src/fetch";
import { createAgentService } from "../src/runtime";
import { createMemoryStore } from "../src/store";
import type { AgentService, AgentEvent } from "../src/types";
import { createNotesHost } from "../examples/notes";
import { consoleNotesProfile } from "../examples/console-profile";
import { RunEvents } from "../src/events";
import { watchRunEvents } from "../src/reconnect";

// No earlier coverage exists for transport admission. These cases prevent a
// denied host request from consuming a body or smuggling authority through JSON.
describe("host Fetch admission", () => {
  test("authentication precedes body consumption and errors reveal no host credentials", async () => {
    let reads = 0;
    const request = new Request("http://host/agent/sessions", {
      method: "POST", headers: { "content-type": "application/json" },
      body: new ReadableStream({ pull() { reads++; throw new Error("must-not-read"); } }, { highWaterMark: 0 }),
      duplex: "half",
    } as RequestInit);
    const handler = createAgentFetchHandler({
      agent: {} as AgentService<never>,
      async authenticate() { throw new Error("private-host-token"); },
    });
    const response = await handler(request);
    expect(response?.status).toBe(401);
    expect(await response?.text()).not.toContain("private-host-token");
    expect(reads).toBe(0);
  });

  test("streamed bodies are byte bounded even without Content-Length", async () => {
    let called = false;
    let cancelled = false;
    const agent = { async createSession() { called = true; throw new Error("must-not-execute"); } } as unknown as AgentService<{}>;
    const handler = createAgentFetchHandler({ agent, authenticate: async () => ({}), maxBodyBytes: 16 });
    const request = new Request("http://host/agent/sessions", {
      method: "POST", headers: { "content-type": "application/json" },
      body: new ReadableStream({
        pull(controller) { controller.enqueue(new Uint8Array(17)); },
        cancel() { cancelled = true; },
      }, { highWaterMark: 0 }),
      duplex: "half",
    } as RequestInit);
    expect((await handler(request))?.status).toBe(413);
    expect(called).toBe(false);
    expect(cancelled).toBe(true);
  });

  test("malformed JSON, wrong media types and actor-bearing JSON never reach service", async () => {
    let calls = 0;
    const agent = { async createSession() { calls++; throw new Error("must-not-execute"); } } as unknown as AgentService<{}>;
    const handler = createAgentFetchHandler({ agent, authenticate: async () => ({}) });
    for (const [contentType, body] of [
      ["text/plain", '{"profile":"console-notes"}'],
      ["application/json", "{"],
      ["application/json", '{"profile":"console-notes","actor":{"subject":"admin"}}'],
      ["application/json", '{"profile":12}'],
    ]) {
      const response = await handler(new Request("http://host/agent/sessions", { method: "POST", headers: { "content-type": contentType }, body }));
      expect(response!.status).toBeGreaterThanOrEqual(400);
    }
    expect(calls).toBe(0);
  });

  test("unmatched requests remain available to the existing host without auth or listener", async () => {
    let authentications = 0;
    const handler = createAgentFetchHandler({
      agent: {} as AgentService<{}>,
      async authenticate() { authentications++; return {}; },
    });
    for (const path of ["/health", "/agentish/sessions", "/agent/unrecognized", "/agent/runs/id/extra"])
      expect(await handler(new Request(`http://host${path}`))).toBeUndefined();
    expect(authentications).toBe(0);
    const response = await handler(new Request("http://host/agent/sessions"));
    expect(response?.status).toBe(405);
    expect(response?.headers.get("allow")).toBe("POST");
  });
});

// Prevent eager accumulation and disconnect-triggered cancellation at the actual
// Web Streams boundary. Runtime event-buffer tests do not cover Response pulling.
test("SSE pulls one live event at a time; connection cancellation only detaches", async () => {
  let nexts = 0;
  let returns = 0;
  let cancellations = 0;
  const events: AsyncIterable<AgentEvent> = {
    [Symbol.asyncIterator]() {
      return {
        async next() {
          nexts++;
          return { done: false as const, value: {
            id: `event-${nexts}`, sequence: nexts, timestamp: 1,
            sessionId: "session", runId: "run", kind: "text_delta", data: "live",
          } satisfies AgentEvent };
        },
        async return() { returns++; return { done: true as const, value: undefined }; },
      };
    },
  };
  const agent = {
    events: async () => events,
    cancelRun: async () => { cancellations++; throw new Error("must-not-cancel"); },
  } as unknown as AgentService<{}>;
  const handler = createAgentFetchHandler({ agent, authenticate: async () => ({}) });
  const response = await handler(new Request("http://host/agent/runs/run/events"));
  expect(nexts).toBe(0);
  const reader = response!.body!.getReader();
  const first = await reader.read();
  expect(new TextDecoder().decode(first.value)).toContain("event: text_delta");
  expect(nexts).toBe(1);
  await reader.cancel();
  expect(returns).toBe(1);
  expect(cancellations).toBe(0);
  const connection = new AbortController();
  const disconnected = await handler(new Request("http://host/agent/runs/run/events", { signal: connection.signal }));
  connection.abort();
  expect((await disconnected!.body!.getReader().read()).done).toBe(true);
  expect(returns).toBe(2);
  expect(nexts).toBe(1);
  expect(cancellations).toBe(0);
});

// The iterator test does not cover the subscription discarded by actual HTTP start.
test("HTTP start detaches its unused event handle so a single-slot stream can attach", async () => {
  let release!: () => void;
  let entered!: () => void;
  const blocked = new Promise<void>(resolve => { release = resolve; });
  const loading = new Promise<void>(resolve => { entered = resolve; });
  const core = createFauxCore({ provider: "fetch-single-slot" });
  core.setResponses([fauxAssistantMessage("Done")]);
  const identity = { subject: "alice", scope: "personal", application: "test", target: "local" };
  const agent = createAgentService({
    store: createMemoryStore(), model: core.getModel(), identity: async () => identity,
    profiles: [{ id: "read", instructions: "Read only.", tools: [] }],
    limits: { eventBuffer: 1 },
    tools: {
      async catalog() { entered(); await blocked; return []; },
      async prepare() { throw new Error("no-tools"); },
      async invoke() { throw new Error("no-tools"); },
    },
  }, { streamFn: core.streamSimple });
  const handler = createAgentFetchHandler({ agent, authenticate: async () => identity });
  try {
    const session = await agent.createSession(identity, "read");
    const started = await handler(new Request(`http://host/agent/sessions/${session.id}/runs`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ text: "Hello" }),
    }));
    expect(started!.status).toBe(202);
    const run = await started!.json();
    await loading;
    const events = await handler(new Request(`http://host/agent/runs/${run.id}/events`));
    expect(events!.status).toBe(200);
    await events!.body!.cancel();
  } finally {
    release();
    await agent.close();
  }
});

// Gateway unit tests do not prove HTTP preserves digest and host actor provenance.
test("host-verified actors and exact pending digest guard real Manage writes through Fetch", async () => {
  const host = await createNotesHost(":memory:");
  const alice = await host.contextFor("alice");
  const bob = await host.contextFor("bob");
  const core = createFauxCore({ provider: "fetch-offline", tokensPerSecond: Infinity });
  core.setResponses([
    fauxAssistantMessage(fauxToolCall("notes_update", {
      id: "welcome", title: "Confirmed through Fetch", body: "Real result", expectedVersion: 1,
    })),
    fauxAssistantMessage("The exact update awaits host confirmation."),
  ]);
  const agent = createAgentService({
    store: createMemoryStore(), identity: host.identity, tools: host.tools,
    model: core.models[0], profiles: [consoleNotesProfile()],
  }, { streamFn: core.streamSimple });
  const handler = createAgentFetchHandler({
    agent,
    async authenticate(request) {
      if (request.headers.get("origin") !== "http://host") throw new Error("csrf-rejected");
      // Test ingress supplies an already verified actor. Production ingress must
      // perform its own credential, scope, target and CSRF checks here.
      return request.headers.get("x-fixture-subject") === "bob" ? bob : alice;
    },
  });
  const confirm = (action: string, digest: string, subject = "alice") => handler(new Request(`http://host/agent/actions/${action}/confirm`, {
    method: "POST", headers: { "origin": "http://host", "content-type": "application/json", "x-fixture-subject": subject },
    body: JSON.stringify({ digest }),
  }));
  try {
    const session = await agent.createSession(alice, "console-notes");
    const handle = await agent.startRun(alice, session.id, "Propose an exact update.");
    expect((await handle.done).status).toBe("completed");
    const action = (await agent.readSession(alice, session.id)).actions.find(item => item.status === "pending")!;
    expect(action).toBeDefined();
    const tampered = await confirm(action.id, `${action.digest}-tampered`);
    expect(tampered?.status).toBe(409);
    expect(await tampered!.json()).toEqual({ error: "confirmation-mismatch" });
    expect((await confirm(action.id, action.digest, "bob"))?.status).toBe(404);
    const before = host.db.query<{ version: number }, []>("SELECT version FROM notes WHERE id = 'welcome'").get()!;
    expect(before.version).toBe(1);
    const confirmed = await confirm(action.id, action.digest);
    expect(confirmed?.status).toBe(200);
    expect((await confirmed!.json()).status).toBe("completed");
    // A transport replay must not create a second SQL side effect, whether the
    // shared service rejects it or returns the already recorded receipt.
    const consumed = await confirm(action.id, action.digest);
    expect(consumed?.status).toBe(409);
    expect(await consumed!.json()).toEqual({ error: "action-completed" });
    expect(host.db.query<{ version: number }, []>("SELECT version FROM notes WHERE id = 'welcome'").get()!.version).toBe(2);
  } finally {
    try { await agent.close(); } finally { await host.close(); }
  }
});

function watchCheckpoint(sequence = 0) {
  const run = { id: "run", sessionId: "session", status: "running" as const, createdAt: 1 };
  return {
    snapshot: {
      session: { id: "session", identity: { subject: "alice", scope: "personal", application: "test", target: "local" }, profile: "read", createdAt: 1 },
      messages: [], runs: [run], actions: [], calls: [],
    },
    run, sequence,
  };
}

test("watch SSE authenticates each connection, sends snapshot first and ignores Last-Event-ID as replay", async () => {
  const source = new RunEvents("session", "run", 4);
  let authentications = 0;
  let calls = 0;
  let suppliedSignal: AbortSignal | undefined;
  const agent = {
    async watchRun(context: { subject: string }, id: string, options?: { signal?: AbortSignal }) {
      calls++;
      expect(context.subject).toBe("alice");
      expect(id).toBe("run");
      suppliedSignal = options?.signal;
      return watchRunEvents(source, () => watchCheckpoint(source.sequence), options);
    },
  } as unknown as AgentService<{ subject: string }>;
  const handler = createAgentFetchHandler({
    agent,
    async authenticate(request) {
      authentications++;
      if (request.headers.get("x-subject") !== "alice") throw new Error("private-auth-token");
      return { subject: "alice" };
    },
  });
  const connection = new AbortController();
  const response = await handler(new Request("http://host/agent/runs/run/watch", {
    headers: { "x-subject": "alice", "last-event-id": "9999" }, signal: connection.signal,
  }));
  expect(response!.status).toBe(200);
  expect(suppliedSignal).toBeDefined();
  source.emit("run_completed");
  const reader = response!.body!.getReader();
  const snapshot = new TextDecoder().decode((await reader.read()).value);
  expect(snapshot).toContain("event: snapshot\n");
  expect(snapshot).toContain('"sequence":0');
  expect(snapshot).toContain('"status":"running"');
  const terminal = new TextDecoder().decode((await reader.read()).value);
  expect(terminal).toContain("id: 1\nevent: run_completed\n");
  expect(new TextDecoder().decode((await reader.read()).value)).toContain("event: resnapshot_required\n");
  expect((await reader.read()).done).toBe(true);
  const denied = await handler(new Request("http://host/agent/runs/run/watch", { headers: { "x-subject": "bob" } }));
  expect(denied!.status).toBe(401);
  expect(await denied!.text()).not.toContain("private-auth-token");
  expect(authentications).toBe(2);
  expect(calls).toBe(1);
});

test("watch SSE never pumps live events before consumption and disconnect only detaches", async () => {
  let nexts = 0;
  let returns = 0;
  let closes = 0;
  let cancellations = 0;
  let finish!: (result: IteratorResult<AgentEvent>) => void;
  const agent = {
    async watchRun() {
      return {
        ...watchCheckpoint(),
        events: {
          [Symbol.asyncIterator]() {
            return {
              next() { nexts++; return new Promise<IteratorResult<AgentEvent>>(resolve => { finish = resolve; }); },
              async return() { returns++; finish?.({ done: true, value: undefined }); return { done: true as const, value: undefined }; },
            };
          },
        },
        async close() { closes++; },
      };
    },
    async cancelRun() { cancellations++; throw new Error("must-not-cancel"); },
  } as unknown as AgentService<{}>;
  const handler = createAgentFetchHandler({ agent, authenticate: async () => ({}) });
  const connection = new AbortController();
  const response = await handler(new Request("http://host/agent/runs/run/watch", { signal: connection.signal }));
  expect(nexts).toBe(0);
  const reader = response!.body!.getReader();
  expect(new TextDecoder().decode((await reader.read()).value)).toContain("event: snapshot");
  expect(nexts).toBe(0);
  const pending = reader.read();
  await Promise.resolve();
  expect(nexts).toBe(1);
  connection.abort();
  expect((await pending).done).toBe(true);
  await Promise.resolve();
  expect(returns).toBe(1);
  expect(closes).toBe(1);
  expect(cancellations).toBe(0);

  const second = await handler(new Request("http://host/agent/runs/run/watch"));
  await second!.body!.cancel();
  expect(returns).toBe(2);
  expect(closes).toBe(2);
  expect(nexts).toBe(1);
  expect(cancellations).toBe(0);
});

test("watch SSE overflow and empty running streams emit explicit safe resnapshot frame", async () => {
  for (const mode of ["overflow", "empty", "failure"]) {
    const source = new RunEvents("session", "run", 1);
    const agent = {
      async watchRun() {
        if (mode === "overflow") return watchRunEvents(source, () => watchCheckpoint());
        return {
          ...watchCheckpoint(),
          events: { async *[Symbol.asyncIterator]() { if (mode === "failure") throw new Error("private-tool-result"); } },
          async close() {},
        };
      },
    } as unknown as AgentService<{}>;
    const handler = createAgentFetchHandler({ agent, authenticate: async () => ({}) });
    const response = await handler(new Request("http://host/agent/runs/run/watch"));
    if (mode === "overflow") { source.emit("text_delta"); source.emit("text_delta"); }
    const text = await response!.text();
    expect(text.indexOf("event: snapshot")).toBeLessThan(text.indexOf("event: resnapshot_required"));
    expect(text).toContain('data: {"code":"resnapshot-required"}');
    expect(text).not.toContain("private-tool-result");
  }
});
