import { describe, expect, test } from "bun:test";
import type { StreamFn } from "@earendil-works/pi-agent-core";
import type { AssistantMessage, Model, SimpleStreamOptions } from "@earendil-works/pi-ai";
import { streamSimple } from "@earendil-works/pi-ai/api/openai-completions";
import { createAgentService, createMemoryStore } from "../src/index";
import type { AgentEvent, Identity, ToolDefinition, ToolSource } from "../src/index";

// Real Pi/OpenAI SDK transport against fixture bytes, not live model/provider acceptance.
const fixtureKey = "fixturekey-provider-transport-only";
const modelId = "offline-transport-fixture";
const oldInput = "private-original-tool-input";
const receipt = { receipt: "human-confirmed-fixture-receipt" };
const identity: Identity = { subject: "alice", scope: "fixture", application: "test", target: "local" };
const write: ToolDefinition = {
  name: "update", description: "Simulated update requiring human confirmation",
  inputSchema: {
    type: "object", properties: { id: { type: "string" } }, required: ["id"], additionalProperties: false,
  },
  target: { application: "test", instance: "local", plugin: "items", operation: "update" },
  effect: "write", confirmation: true,
};

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(done => { resolve = done; });
  return { promise, resolve };
}

function rejectCredentialEnv(env: Record<string, string | undefined> = process.env) {
  // The SDK can read these even when apiKey and baseURL are explicit.
  for (const name of [
    "OPENAI_API_KEY", "OPENAI_ADMIN_KEY", "OPENAI_ORG_ID", "OPENAI_PROJECT_ID",
    "OPENAI_WEBHOOK_SECRET", "OPENAI_BASE_URL",
  ]) {
    if (env[name]) throw new Error("provider fixture refuses OpenAI credential/config environment");
  }
}

function requireFixtureUrl(value: string | URL, origin: string) {
  const url = new URL(value);
  if (url.protocol !== "http:" || url.hostname !== "127.0.0.1" ||
      url.origin !== origin || url.pathname !== "/v1/chat/completions" ||
      url.username || url.password || url.search || url.hash) {
    throw new Error("provider fixture refuses non-loopback destination");
  }
}

function chunk(delta: Record<string, unknown>, finishReason: string | null = null) {
  return {
    id: "chatcmpl-local-fixture", object: "chat.completion.chunk", created: 1, model: modelId,
    choices: [{ index: 0, delta, finish_reason: finishReason }],
  };
}

const callChunks = [
  chunk({
    role: "assistant", tool_calls: [{
      index: 0, id: "call_local_update", type: "function", function: { name: "update", arguments: '{"id":' },
    }],
  }),
  chunk({ tool_calls: [{ index: 0, function: { arguments: `${JSON.stringify(oldInput)}}` } }] }),
];
const encodeEvent = (event: unknown) => new TextEncoder().encode(`data: ${JSON.stringify(event)}\n\n`);
function sse(events: unknown[]) {
  return new Response(new Blob([
    ...events.map(encodeEvent), "data: [DONE]\n\n",
  ]), { headers: { "content-type": "text/event-stream" } });
}
const answer = (text: string) => sse([
  chunk({ role: "assistant", content: text }),
  chunk({}, "stop"),
]);

interface HttpPayload {
  model: string;
  stream: boolean;
  messages: {
    role: string;
    content: unknown;
    tool_calls?: { id: string; type: string; function: { name: string; arguments: string } }[];
    tool_call_id?: string;
  }[];
  tools: { type: string; function: { name: string; parameters: unknown } }[];
}

function fixture(
  respond: (requestNumber: number) => Response,
  observe?: SimpleStreamOptions["onProviderStreamEvent"],
) {
  rejectCredentialEnv();
  const requests: HttpPayload[] = [];
  const failures: unknown[] = [];
  const server = Bun.serve({
    hostname: "127.0.0.1", port: 0,
    async fetch(request) {
      try {
        requireFixtureUrl(request.url, server.url.origin);
        expect(request.method).toBe("POST");
        expect(request.headers.get("authorization")).toBe(`Bearer ${fixtureKey}`);
        const payload = await request.json() as HttpPayload;
        expect(payload.model).toBe(modelId);
        expect(payload.stream).toBe(true);
        requests.push(payload);
        return respond(requests.length);
      } catch (error) {
        failures.push(error);
        return Response.json({ error: { message: "fixture-request-rejected" } }, { status: 400 });
      }
    },
  });
  const model: Model<"openai-completions"> = {
    id: modelId, name: "Offline HTTP fixture", api: "openai-completions", provider: "openai",
    baseUrl: `${server.url.origin}/v1`, reasoning: false, input: ["text"],
    contextWindow: 32_768, maxTokens: 1024,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
  };
  // Guard only; response bodies still come from native HTTP and the published SDK.
  const preconnect: typeof fetch.preconnect = (url, options) => {
    requireFixtureUrl(url, server.url.origin);
    fetch.preconnect(url, options);
  };
  const loopbackFetch: typeof fetch = Object.assign((input: string | URL | Request, init?: RequestInit) => {
    rejectCredentialEnv();
    requireFixtureUrl(input instanceof Request ? input.url : input, server.url.origin);
    const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
    if (headers.get("authorization") !== `Bearer ${fixtureKey}` ||
        headers.has("openai-organization") || headers.has("openai-project")) {
      throw new Error("provider fixture refuses non-fixture credentials");
    }
    return fetch(input, { ...init, redirect: "error" });
  }, { preconnect });
  const terminals: AssistantMessage[] = [];
  const signals: AbortSignal[] = [];
  const streamFn: StreamFn = (requestedModel, context, options) => {
    rejectCredentialEnv();
    if (requestedModel.api !== model.api || requestedModel.baseUrl !== model.baseUrl ||
        options?.apiKey !== fixtureKey) throw new Error("provider fixture configuration rejected");
    if (options.signal) signals.push(options.signal);
    const stream = streamSimple(model, context, {
      ...options, fetch: loopbackFetch, maxRetries: 0, cacheRetention: "none",
      onProviderStreamEvent: observe,
    });
    void stream.result().then(message => { terminals.push(message); });
    return stream;
  };
  const store = createMemoryStore();
  const invocations: boolean[] = [];
  const tools: ToolSource<Identity> = {
    async catalog() { return [write]; },
    async prepare(name, input) {
      expect(name).toBe(write.name);
      return { definition: write, input, precondition: "fixture-v1" };
    },
    async invoke(_prepared, _context, _identity, _signal, confirmed) {
      invocations.push(confirmed);
      return receipt;
    },
  };
  const service = createAgentService({
    store, tools, identity: async context => context, model,
    profiles: [{ id: "default", instructions: "Use only listed business tools.", tools: ["update"] }],
    limits: { timeoutMs: 5000, maxModelTurns: 3, maxToolCalls: 2 },
  }, { streamFn, getApiKey: async () => fixtureKey });
  return {
    service, store, requests, terminals, signals, invocations, failures,
    origin: server.url.origin,
    async close() {
      try { await service.close(); }
      finally { store.close(); await server.stop(true); }
    },
  };
}

async function collect(events: AsyncIterable<AgentEvent>) {
  const result: AgentEvent[] = [];
  for await (const event of events) result.push(event);
  return result;
}

describe("real Pi 1.1.0 / OpenAI loopback transport (no public provider)", () => {
  test("HTTP tool call proposes only; human confirmation makes no request; next user run sees receipt", async () => {
    const f = fixture(number => {
      if (number === 1) return sse([...callChunks, chunk({}, "tool_calls")]);
      if (number === 2) return sse([
        chunk({ role: "assistant", content: "Awaiting approval. fixturekey-" }),
        chunk({ content: "provider-transport-only" }), chunk({}, "stop"),
      ]);
      if (number === 3) return answer("Receipt observed.");
      throw new Error("unexpected model continuation");
    });
    try {
      expect(() => requireFixtureUrl("https://api.openai.com/v1/chat/completions", f.origin)).toThrow("non-loopback");
      expect(() => rejectCredentialEnv({ OPENAI_API_KEY: "not-a-real-key" })).toThrow("credential/config");
      const session = await f.service.createSession(identity, "default");
      const first = await f.service.startRun(identity, session.id, "Please update the item.");
      const events = collect(first.events);
      expect((await first.done).status).toBe("completed");
      const pending = await f.service.readSession(identity, session.id);
      expect(pending.actions).toHaveLength(1);
      const action = pending.actions[0]!;
      expect(action.status).toBe("pending");
      expect(action.prepared.input).toEqual({ id: oldInput });
      expect(pending.calls[0]?.modelCallId).toBe("call_local_update");
      expect(f.invocations).toEqual([]);
      expect(f.requests).toHaveLength(2);
      expect(f.requests[0]!.tools[0]!.function.parameters).toEqual(write.inputSchema);
      const continuation = f.requests[1]!.messages;
      expect(continuation.find(message => message.role === "assistant")?.tool_calls).toEqual([{
        id: "call_local_update", type: "function", function: { name: "update", arguments: JSON.stringify({ id: oldInput }) },
      }]);
      const toolResult = continuation.find(message => message.role === "tool")!;
      expect(toolResult.tool_call_id).toBe("call_local_update");
      expect(JSON.parse(toolResult.content as string)).toEqual({
        status: "pending_confirmation", actionId: action.id,
      });
      const publicEvents = await events;
      expect(publicEvents.map(event => event.kind)).toContain("pending_action");
      expect(JSON.stringify({ pending, publicEvents })).not.toContain(fixtureKey);
      expect(publicEvents.filter(event => event.kind === "text_delta")
        .map(event => (event.data as { text: string }).text).join("")).toBe("Awaiting approval. [redacted]");

      expect((await f.service.confirmAction(identity, action.id, action.digest)).result).toEqual(receipt);
      expect(f.invocations).toEqual([true]);
      expect(f.requests).toHaveLength(2);
      await expect(f.service.confirmAction(identity, action.id, action.digest)).rejects.toThrow("action-completed");
      expect(f.requests).toHaveLength(2);
      const confirmed = await f.service.readSession(identity, session.id);
      expect(confirmed.calls[0]?.status).toBe("completed");
      expect(confirmed.messages.filter(message => message.source?.kind === "action-outcome")).toHaveLength(1);

      const next = await f.service.startRun(identity, session.id, "What was the confirmed outcome?");
      const nextEvents = collect(next.events);
      expect((await next.done).status).toBe("completed");
      await nextEvents;
      expect(f.requests).toHaveLength(3);
      const messages = f.requests[2]!.messages;
      const outcome = messages.find(message => message.role === "user" &&
        typeof message.content === "string" && message.content.includes('"source":"action-outcome"'));
      expect(JSON.parse(outcome!.content as string)).toMatchObject({
        source: "action-outcome", actionId: action.id, callId: action.callId,
        toolName: "update", status: "completed", result: receipt,
      });
      expect(messages.some(message => message.role === "tool" || message.tool_calls || message.tool_call_id)).toBe(false);
      expect(JSON.stringify(messages)).not.toContain(oldInput);
      expect(JSON.stringify(messages)).not.toContain('"prepared"');
      expect(JSON.stringify(messages)).not.toContain(fixtureKey);
      expect(f.invocations).toEqual([true]);
      expect(f.terminals.map(message => message.stopReason)).toEqual(["toolUse", "stop", "stop"]);
      expect(f.failures).toEqual([]);
    } finally { await f.close(); }
  });

  test("cancel and close drain the real adapter's blocked SSE observer without tool side effects", async () => {
    const entered = deferred(), release = deferred(), aborted = deferred();
    let controller: ReadableStreamDefaultController<Uint8Array> | undefined;
    let observed = 0;
    const f = fixture(() => new Response(new ReadableStream<Uint8Array>({
      start(stream) {
        controller = stream;
        stream.enqueue(encodeEvent(callChunks[0]));
        stream.enqueue(encodeEvent(chunk({ tool_calls: [{ index: 0, function: { arguments: '"partial' } }] })));
        // No finish_reason or [DONE]: cancellation, not fixture EOF, must stop the SDK.
      },
      cancel() { controller = undefined; },
    }), { headers: { "content-type": "text/event-stream" } }), async () => {
      if (++observed === 2) {
        entered.resolve();
        await release.promise;
      }
    });
    try {
      const session = await f.service.createSession(identity, "default");
      const handle = await f.service.startRun(identity, session.id, "Propose an update.");
      const events = collect(handle.events);
      await Promise.race([
        entered.promise,
        handle.done.then(() => { throw new Error("adapter finished before the SSE barrier"); }),
      ]);
      expect(f.requests).toHaveLength(1);
      const signal = f.signals[0]!;
      signal.addEventListener("abort", aborted.resolve, { once: true });
      let done = false, cancelled = false, closed = false;
      void handle.done.then(() => { done = true; });
      const cancellation = f.service.cancelRun(identity, handle.run.id).then(run => { cancelled = true; return run; });
      await aborted.promise;
      expect((await f.service.getRun(identity, handle.run.id)).status).toBe("running");
      const closing = f.service.close().then(() => { closed = true; });
      expect(signal.aborted).toBe(true);
      expect([done, cancelled, closed]).toEqual([false, false, false]);
      expect(f.terminals).toEqual([]);
      release.resolve();
      expect((await cancellation).status).toBe("cancelled");
      await closing;
      expect((await handle.done).error).toBe("cancelled");
      expect([done, cancelled, closed]).toEqual([true, true, true]);
      expect(f.terminals.map(message => message.stopReason)).toEqual(["aborted"]);
      expect(f.terminals[0]!.content.some(block => block.type === "toolCall")).toBe(true);
      const snapshot = f.store.transaction(tx => ({
        calls: tx.list("calls", session.id), actions: tx.list("actions", session.id),
      }));
      expect(snapshot).toEqual({ calls: [], actions: [] });
      expect(f.invocations).toEqual([]);
      expect((await events).map(event => event.kind)).toContain("run_cancelled");
      expect(f.requests).toHaveLength(1);
      expect(f.failures).toEqual([]);
    } finally {
      release.resolve();
      try { controller?.close(); } catch { /* The disconnected HTTP stream may already be closed. */ }
      await f.close();
    }
  });

  test("HTTP provider error is parsed by the SDK but only safe codes reach public and durable state", async () => {
    const privateError = "private-fixture-provider-error";
    const f = fixture(() => Response.json({
      error: { message: `${privateError}: ${fixtureKey}`, type: "invalid_request_error", code: "fixture_bad_request" },
    }, { status: 400 }));
    try {
      const session = await f.service.createSession(identity, "default");
      const handle = await f.service.startRun(identity, session.id, "Make one request.");
      const events = collect(handle.events);
      const run = await handle.done;
      const publicEvents = await events;
      expect(run.status).toBe("failed");
      expect(run.error).toBe("provider-failed");
      expect(publicEvents.filter(event => event.kind === "safe_error").map(event => event.data)).toEqual([
        { code: "provider-failed" },
      ]);
      expect(f.terminals[0]?.stopReason).toBe("error");
      expect(f.terminals[0]?.errorMessage).toContain(privateError);
      expect(f.terminals[0]?.errorMessage).toContain(fixtureKey);
      const snapshot = await f.service.readSession(identity, session.id);
      const durable = f.store.transaction(tx => ({
        runs: tx.list("runs", session.id), messages: tx.list("messages", session.id),
        calls: tx.list("calls", session.id), actions: tx.list("actions", session.id),
      }));
      const exposed = JSON.stringify({ run, publicEvents, snapshot, durable });
      expect(exposed).not.toContain(privateError);
      expect(exposed).not.toContain(fixtureKey);
      expect(exposed).not.toContain("fixture_bad_request");
      expect(snapshot.calls).toEqual([]);
      expect(snapshot.actions).toEqual([]);
      expect(f.invocations).toEqual([]);
      expect(f.requests).toHaveLength(1);
      expect(f.failures).toEqual([]);
    } finally { await f.close(); }
  });
});
