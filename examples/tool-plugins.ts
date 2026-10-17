import assert from "node:assert/strict";
import { mkdtemp, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createHash } from "node:crypto";
import { defineApp, definePlugin, startApp } from "@lenso/core";
import { defineOperation } from "@lenso/engine/operations";
import { createFauxCore, fauxAssistantMessage, fauxToolCall, type Context as TranscriptContext } from "@earendil-works/pi-ai";
import { z } from "zod";
import { combineToolSources, createAgentService, createMemoryStore, type Identity } from "../src/index";
import { createManageToolSource } from "../src/manage";

// The provider is scripted; both plugins execute real filesystem operations in an owned temp directory.
const directory = await mkdtemp(join(tmpdir(), "lenso-tool-plugins-"));
const body = "Studio local preview\n".repeat(20_000);
let listCalls = 0, checksumCalls = 0, rawBytes = 0;
const studio = definePlugin({
  id: "studio-files",
  setup: () => ({
    async list(input: { cursor: number }) {
      listCalls++;
      const names = (await readdir(directory)).sort();
      const rows = await Promise.all(names.slice(input.cursor).map(async name => ({
        name, body: await readFile(join(directory, name), "utf8"),
      })));
      const result = { rows, cursor: input.cursor };
      rawBytes = Math.max(rawBytes, Buffer.byteLength(JSON.stringify(result)));
      return result;
    },
  }),
});
const relay = definePlugin({
  id: "relay-files",
  setup: () => ({
    async checksum(input: { name: string }) {
      checksumCalls++;
      const bytes = await readFile(join(directory, input.name));
      return { name: input.name, bytes: bytes.length, sha256: createHash("sha256").update(bytes).digest("hex") };
    },
  }),
});
try {
  await writeFile(join(directory, "preview.txt"), body);
  await writeFile(join(directory, "summary.txt"), "Relay local summary\n");
  const running = await startApp(defineApp({ instanceId: "local-tools", plugins: [studio, relay] }));
  const store = createMemoryStore();
  const identity: Identity = { subject: "offline-user", scope: "local", application: "studio-relay", target: running.instanceId };
  const list = defineOperation({
    plugin: studio, method: "list", description: "List local files; use nextCursor for the next page.",
    // A domain refinement exercises server-side validation after JSON Schema validation.
    input: z.object({ cursor: z.number().int().refine(value => value >= 0 && value <= 1) }), effect: "read",
  });
  const checksum = defineOperation({
    plugin: relay, method: "checksum", description: "Compute the real SHA-256 of an allowed local file.",
    input: z.object({ name: z.enum(["preview.txt", "summary.txt"]) }), effect: "read",
  });
  const resultSchema = z.object({ rows: z.array(z.object({ name: z.string(), body: z.string() })), cursor: z.number() });
  const common = {
    running, application: identity.application, plugins: [studio, relay],
    authorize: async (_operation: unknown, _input: unknown, host: Identity, actor: Identity) =>
      host === identity && actor.subject === identity.subject && actor.scope === identity.scope,
    binding: async () => ({}),
  };
  const tools = combineToolSources([
    { source: createManageToolSource({ ...common, selections: [{
      name: "studio_list", operation: list,
      projectResult: raw => {
        const result = resultSchema.parse(raw);
        return {
          items: result.rows.slice(0, 1).map(row => ({ name: row.name, bytes: Buffer.byteLength(row.body) })),
          nextCursor: result.rows.length > 1 ? result.cursor + 1 : null,
        };
      },
    }] }), tools: ["studio_list"] },
    { source: createManageToolSource({ ...common, selections: [{ name: "relay_checksum", operation: checksum }] }), tools: ["relay_checksum"] },
  ]);
  const core = createFauxCore({ models: [{ id: "offline-tools" }] });
  let observedError: unknown;
  const pageSchema = z.object({ items: z.array(z.object({ name: z.string(), bytes: z.number() })), nextCursor: z.number().nullable() });
  let firstPage: z.infer<typeof pageSchema>;
  function lastResult(context: TranscriptContext): unknown {
    const message = context.messages.findLast(message => message.role === "toolResult");
    assert(message?.role === "toolResult");
    const block = message.content[0];
    assert(block?.type === "text");
    return JSON.parse(block.text);
  }
  core.setResponses([
    fauxAssistantMessage(fauxToolCall("studio_list", { cursor: -1 })),
    context => {
      observedError = lastResult(context);
      assert.deepEqual(observedError, { error: { code: "invalid-input", recovery: "correct-input" } });
      return fauxAssistantMessage(fauxToolCall("studio_list", { cursor: 0 }));
    },
    context => {
      firstPage = pageSchema.parse(lastResult(context));
      assert.equal(firstPage.items[0]!.name, "preview.txt");
      assert.equal(firstPage.items[0]!.bytes, Buffer.byteLength(body));
      assert.equal(firstPage.nextCursor, 1);
      return fauxAssistantMessage(fauxToolCall("studio_list", { cursor: firstPage.nextCursor }));
    },
    context => {
      const page = pageSchema.parse(lastResult(context));
      assert.equal(page.items[0].name, "summary.txt");
      assert.equal(page.nextCursor, null);
      return fauxAssistantMessage(fauxToolCall("relay_checksum", { name: firstPage.items[0]!.name }));
    },
    context => {
      const result = z.object({ name: z.string(), bytes: z.number(), sha256: z.string() }).parse(lastResult(context));
      assert.equal(result.sha256, createHash("sha256").update(body).digest("hex"));
      return fauxAssistantMessage(`Verified ${result.name}: ${result.bytes} bytes, SHA-256 ${result.sha256}.`);
    },
  ]);
  const agent = createAgentService({
    store, identity: async host => host, tools, model: core.getModel(),
    profiles: [{ id: "local-tools", instructions: "Use local tools and page through results.", tools: ["studio_list", "relay_checksum"] }],
    limits: { maxOutputBytes: 16_384 },
  }, { streamFn: core.streamSimple });
  try {
    const session = await agent.createSession(identity, "local-tools");
    const run = await agent.startRun(identity, session.id, "List and verify the preview file.");
    const events = (async () => { for await (const _event of run.events) { /* drain */ } })();
    assert.equal((await run.done).status, "completed");
    await events;
    const snapshot = await agent.readSession(identity, session.id);
    assert.equal(listCalls, 2); // Invalid input never reached the business service.
    assert.equal(checksumCalls, 1);
    assert.equal(snapshot.calls.length, 3);
    assert(snapshot.calls.every(call => call.status === "completed"));
    assert.deepEqual(snapshot.calls[0]!.result, firstPage!);
    const savedError = snapshot.messages.find(message => message.value.role === "toolResult" && message.value.isError);
    assert(savedError?.value.role === "toolResult");
    assert.deepEqual(JSON.parse((savedError.value.content[0] as { text: string }).text), observedError);
    assert(rawBytes > 262_144);
    console.log(JSON.stringify({
      status: "completed", rawBytes, projectedBytes: Buffer.byteLength(JSON.stringify(firstPage!)),
      listCalls, checksumCalls, error: observedError, result: snapshot.calls.at(-1)!.result,
    }, null, 2));
  } finally {
    await agent.close();
    store.close();
    await running.stop();
  }
} finally {
  await rm(directory, { recursive: true, force: true });
}
