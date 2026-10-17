import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import { definePlugin, startApp } from "@lenso/core";
import { createFauxCore, fauxAssistantMessage, fauxToolCall } from "@earendil-works/pi-ai";
import { combineToolSources, createMemoryStore, type Identity } from "../src/index";
import { createAgentPlugin } from "../src/plugin";
import { createMcpToolPlugin } from "../src/mcp";
import { createContextTransform } from "../src/context";

// A deliberately fixed local tool contract, not a general JSON Schema validator.
const schema = { type: "object", properties: { value: { type: "string" } }, required: ["value"], additionalProperties: false };
if (process.argv.includes("--server")) {
  createInterface({ input: process.stdin }).on("line", line => {
    const { id, method, params } = JSON.parse(line);
    const reply = (result: unknown) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, result }) + "\n");
    if (method === "initialize") reply({ protocolVersion: params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: "local-echo", version: "1" } });
    else if (method === "tools/list") reply({ tools: [{ name: "echo", inputSchema: schema }] });
    else if (method === "tools/call") reply({ content: [], structuredContent: { value: params.arguments.value } });
  });
} else {
  // All dependencies are created during setup; no mutable outer provider/source handles.
  const provider = definePlugin({ id: "fake-provider", setup() {
    const core = createFauxCore({ tokensPerSecond: Infinity });
    core.setResponses([
      fauxAssistantMessage(fauxToolCall("local_echo", { value: "" })),
      context => {
        assert(JSON.stringify(context.messages).includes("correct-input"));
        return fauxAssistantMessage(fauxToolCall("local_echo", { value: "hello" }));
      },
      context => {
        assert(JSON.stringify(context.messages).includes("hello"));
        return fauxAssistantMessage("Echo completed locally.");
      },
    ]);
    return core;
  } });
  const storage = definePlugin({ id: "memory", setup(context) {
    const store = createMemoryStore(); context.onCleanup(() => store.close()); return store;
  } });
  const mcp = createMcpToolPlugin<Identity>({ id: "mcp", options: context => ({
    application: "demo", instance: context.instanceId, sourceId: "local-mcp",
    server: { command: process.execPath, args: [import.meta.path, "--server"] },
    selections: [{ name: "local_echo", tool: "echo", effect: "read" }],
    // Demo identity only. Production hosts must authenticate their caller themselves.
    authorize: async (_selection, _input, host, actor) => host.subject === "local-user" && actor.subject === host.subject,
    validateInput(input, advertisedSchema) {
      assert.deepEqual(advertisedSchema, schema); // Reject schema drift; never silently skip validation.
      assert.equal(typeof input.value, "string");
      assert((input.value as string).trim().length > 0);
      assert.deepEqual(Object.keys(input), ["value"]);
    },
  }) });
  let projections = 0;
  const lifecycle: string[] = [];
  const agentPlugin = createAgentPlugin<Identity>({
    id: "assistant", requires: [provider, storage, mcp],
    options: context => ({
      store: context.get(storage), model: context.get(provider).getModel(), identity: async actor => actor,
      tools: combineToolSources([context.get(mcp)]),
      profiles: [{ id: "echo", instructions: "Echo using the local tool.", tools: context.get(mcp).tools }],
    }),
    pi: context => ({
      streamFn: context.get(provider).streamSimple,
      hooks: [{ beforeRun: async () => { lifecycle.push("before"); }, afterRun: async () => { lifecycle.push("after"); } }],
      transformContext: createContextTransform({ mode: "trim", budget: {
        maxTokens: 16_000, reserveTokens: 4_000, maxBytes: 12_000,
        // Conservative fixture estimate, not a tokenizer guarantee for real models.
        estimateTokens: messages => Buffer.byteLength(JSON.stringify(messages)),
      } }, () => { projections++; }),
    }),
  });
  const app = await startApp({ instanceId: "local", plugins: [provider, storage, mcp, agentPlugin] });
  try {
    const agent = app.get(agentPlugin);
    const actor: Identity = { subject: "local-user", scope: "demo", application: "demo", target: app.instanceId };
    const session = await agent.createSession(actor, "echo");
    const run = await agent.startRun(actor, session.id, "Echo hello.");
    const result = await run.done;
    assert.equal(result.status, "completed", JSON.stringify(await agent.readSession(actor, session.id)));
    const snapshot = await agent.readSession(actor, session.id);
    assert(projections > 0);
    assert.deepEqual(lifecycle, ["before", "after"]);
    assert.equal(snapshot.calls.length, 1); // Bad input never reached MCP execution.
    assert.equal(snapshot.calls[0]!.status, "completed");
    assert.deepEqual(snapshot.calls[0]!.result, { content: [], structuredContent: { value: "hello" } });
    console.log(JSON.stringify({ status: result.status, calls: snapshot.calls.length, recovery: "correct-input" }));
  } finally {
    await app.stop(); // Drains agent first, then closes MCP and store dependencies.
  }
}
