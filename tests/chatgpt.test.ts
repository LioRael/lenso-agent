import { test, expect } from "bun:test";
import { chatGPTPayload, createChatGPTProvider } from "../src/chatgpt";
import { createAgentService, createMemoryStore, type ToolDefinition } from "../src/index";

// No OAuth or network: actual Pi/OpenAI transport consumes injected SSE fixture bytes.
const token = "siwc-fixture-not-a-credential";
const transcript: any = { messages: [{ role: "system", content: "Fixture instructions", timestamp: 1, toolsAdded: [{ name: "studio_assets", description: "read", parameters: { type: "object", properties: {} } }] }, { role: "user", content: "Fixture input", timestamp: 1 }] };
function sse(type = "response.completed") {
  const output = [{ id: "msg_fixture", type: "message", role: "assistant", status: "completed", content: [{ type: "output_text", text: '{"text":"fixture draft"}', annotations: [] }] }];
  return new Response(`event: response.output_item.done\ndata: ${JSON.stringify({type:"response.output_item.done",output_index:0,item:output[0]})}\n\nevent: ${type}\ndata: ${JSON.stringify({ type, response: { id: "resp_fixture", status: type === "response.completed" ? "completed" : "incomplete", output, usage: { input_tokens: 2, output_tokens: 3, total_tokens: 5 } } })}\n\n`, { headers: { "content-type": "text/event-stream" } });
}
function provider(fetcher: any, getAccess: any = async () => ({ accessToken: token, registrationId: "registration", modelId: "account-visible-model" })) {
  return createChatGPTProvider({ owner: "owner", registrationId: "registration", modelId: "account-visible-model", getAccess, fetch: fetcher });
}
async function result(p: ReturnType<typeof provider>, signal?: AbortSignal) {
  const apiKey = await p.pi.getApiKey!("openai");
  const stream = await p.pi.streamFn(p.model, transcript, { apiKey, signal, maxTokens: 99 });
  for await (const _event of stream) {}
  return stream.result();
}
test("SIWC payload retains only supported stateless stream fields and namespaced host tools", () => {
  const payload = chatGPTPayload({ input: [{ role: "system", content: "instructions" }], model: "visible", tools: [{ type: "function", name: "read" }], max_output_tokens: 10, temperature: 0, previous_response_id: "old", store: true, stream: false });
  expect(payload.store).toBe(false); expect(payload.stream).toBe(true); expect(payload.input[0].role).toBe("developer");
  expect(payload.tools[0].type).toBe("namespace"); expect(payload.tools[0].tools[0].name).toBe("read");
  expect(payload).not.toHaveProperty("max_output_tokens"); expect(payload).not.toHaveProperty("temperature"); expect(payload).not.toHaveProperty("previous_response_id");
  expect(() => chatGPTPayload({ input: [], tools: [{ type: "image_generation" }] })).toThrow();
});
test("official public Responses transport sends selected token and accepts only completed inference", async () => {
  let calls = 0;
  const p = provider(async (url: any, init: RequestInit) => {
    calls++; const request = new Request(url, init); expect(request.url).toBe("https://api.openai.com/v1/responses");
    const headers = request.headers; expect(headers.get("authorization")).toBe(`Bearer ${token}`);
    expect(headers.has("openai-organization")).toBe(false); expect(headers.has("openai-project")).toBe(false);
    const body = await request.json() as any; expect(body.store).toBe(false); expect(body.stream).toBe(true); expect(body.model).toBe("account-visible-model");
    expect(body.tools[0].type).toBe("namespace"); expect(body).not.toHaveProperty("max_output_tokens");
    expect(init.redirect).toBe("error"); return sse();
  });
  const completed = await result(p); expect(completed.stopReason).toBe("stop"); expect(completed.content).toContainEqual(expect.objectContaining({type:"text",text:'{"text":"fixture draft"}'})); expect(calls).toBe(1);
  const incomplete = await result(provider(async () => sse("response.incomplete")));
  expect(incomplete.stopReason).toBe("error");
  const interrupted = await result(provider(async () => new Response("event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n", { headers: { "content-type": "text/event-stream" } })));
  expect(interrupted.stopReason).toBe("error");
});
test("account/model changes fail before transport; AbortSignal reaches the active stream without retry", async () => {
  const stale = provider(() => { throw new Error("must not run"); }, async () => ({ accessToken: token, registrationId: "different", modelId: "account-visible-model" }));
  await expect(stale.pi.getApiKey!("openai")).rejects.toThrow("Connection changed");
  let calls = 0, entered!: () => void;
  const started = new Promise<void>(resolve => entered = resolve), controller = new AbortController();
  const pending = result(provider(async (_url: any, init: RequestInit) => { calls++; entered(); return new Promise((_resolve, reject) => init.signal!.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")), { once: true })); }), controller.signal);
  await started; controller.abort(); expect((await pending).stopReason).toBe("aborted"); expect(calls).toBe(1);
});

test("construction is inert, selection is snapshotted, and implicit credentials are refused", async () => {
  let calls = 0;
  const options = { owner: "owner", registrationId: "registration", modelId: "selected",
    getAccess: async (owner: string, input: { registrationId: string }) => {
      calls++; expect(owner).toBe("owner"); expect(input.registrationId).toBe("registration");
      return { accessToken: token, registrationId: "registration", modelId: "selected" };
    }, fetch: (async (_url: RequestInfo | URL): Promise<Response> => { throw new Error("unexpected HTTP"); }) as typeof fetch };
  const p = createChatGPTProvider(options);
  expect(calls).toBe(0);
  options.owner = "other"; options.registrationId = "other"; options.modelId = "other";
  expect(await p.pi.getApiKey("openai")).toBe(token);
  expect(() => p.pi.streamFn(p.model, transcript, {})).toThrow("Explicit selected");
  await expect(p.pi.getApiKey("other")).rejects.toThrow("Provider mismatch");
  const changedModel = provider(options.fetch, async () => ({ accessToken: token, registrationId: "registration", modelId: "other" }));
  await expect(changedModel.pi.getApiKey("openai")).rejects.toThrow("Connection changed");
  expect(() => createChatGPTProvider({ ...options, modelId: "" })).toThrow();
  expect(() => createChatGPTProvider({ ...options, toolNamespace: "invalid.namespace" })).toThrow();
});

test("failed terminal events and HTTP rate limits never retry or become successful output", async () => {
  let calls = 0;
  const p = provider(async () => {
    calls++;
    return new Response('event: response.failed\ndata: {"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"Fixture limit"}}}\n\n', { headers: { "content-type": "text/event-stream" } });
  });
  expect((await result(p)).stopReason).toBe("error");
  expect(calls).toBe(1);
  calls = 0;
  expect((await result(provider(async () => { calls++; return new Response('{"error":{"message":"Fixture limit"}}', { status: 429 }); }))).stopReason).toBe("error");
  expect(calls).toBe(1);
});

test("redaction remembers verified rotated access only, stays private and returns detached snapshots", async () => {
  let access = { accessToken: "first-synthetic-access", registrationId: "registration", modelId: "account-visible-model" };
  const p = provider(async () => sse(), async () => access);
  expect(p.secrets()).toEqual([]);
  await p.pi.getApiKey("openai");
  const snapshot = p.secrets();
  (snapshot as string[]).push("caller-injected");
  access = { ...access, accessToken: "rotated-synthetic-access" };
  await p.pi.getApiKey("openai");
  expect(p.secrets()).toEqual(["first-synthetic-access", "rotated-synthetic-access"]);
  access = { ...access, accessToken: "rejected-synthetic-access", registrationId: "other" };
  await expect(p.pi.getApiKey("openai")).rejects.toThrow("Connection changed");
  expect(p.secrets()).not.toContain("rejected-synthetic-access");
  expect(JSON.stringify(p)).not.toContain("synthetic-access");
});

test("two owners use isolated credentials and namespaced tools through the real Agent gateway", async () => {
  await Promise.all(["alice", "bob"].map(async owner => {
    let requests = 0, invoked = 0, accessReads = 0;
    const identity = { subject: owner, scope: owner, application: "fixture", target: "local" };
    const definition: ToolDefinition = { name: "lookup", description: "Read one fixture", inputSchema: { type: "object", properties: {} },
      target: { application: "fixture", instance: "local", plugin: "notes", operation: "lookup" }, effect: "read", confirmation: false };
    const p = createChatGPTProvider({ owner, registrationId: owner, modelId: `${owner}-model`, toolNamespace: "notes",
      async getAccess(actualOwner, input) {
        accessReads++; expect(actualOwner).toBe(owner); expect(input.registrationId).toBe(owner);
        return { accessToken: `${owner}-synthetic-access`, registrationId: owner, modelId: `${owner}-model` };
      },
      fetch: (async (url: any, init: RequestInit) => {
        requests++;
        const request = new Request(url, init), body = await request.json() as any;
        expect(request.headers.get("authorization")).toBe(`Bearer ${owner}-synthetic-access`);
        expect(body.model).toBe(`${owner}-model`); expect(body.tools[0].name).toBe("notes");
        if (requests === 2) {
          expect(body.input.find((item: any) => item.type === "function_call_output").output).toContain(`${owner}-result`);
          return sse();
        }
        const item = { id: "fc_fixture", type: "function_call", call_id: "call_fixture", name: "lookup", namespace: "notes", arguments: "{}", status: "completed" };
        return new Response(`event: response.output_item.done\ndata: ${JSON.stringify({ type: "response.output_item.done", output_index: 0, item })}\n\nevent: response.completed\ndata: ${JSON.stringify({ type: "response.completed", response: { id: "fixture", status: "completed", output: [item], usage: { input_tokens: 1, output_tokens: 1 } } })}\n\n`, { headers: { "content-type": "text/event-stream" } });
      }) as typeof fetch,
    });
    const agent = createAgentService({ model: p.model, secrets: p.secrets, store: createMemoryStore(), identity: async () => identity,
      profiles: [{ id: "read", instructions: "Read the fixture", tools: ["lookup"] }],
      tools: { catalog: async () => [definition], prepare: async (_name, input) => ({ definition, input, precondition: null }),
        invoke: async () => { invoked++; return `${owner}-result`; } },
    }, p.pi);
    try {
      const session = await agent.createSession({}, "read");
      const run = await agent.startRun({}, session.id, "Read fixture");
      expect((await run.done).status).toBe("completed");
      expect(requests).toBe(2); expect(invoked).toBe(1); expect(accessReads).toBe(2);
      const saved = await agent.readSession({}, session.id);
      expect(saved.calls[0].status).toBe("completed");
      expect(JSON.stringify(saved)).not.toContain(`${owner}-synthetic-access`);
    } finally { await agent.close(); }
  }));
});
