import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createFauxCore, fauxAssistantMessage, fauxToolCall } from "@earendil-works/pi-ai";
import { createAgentService, createMemoryStore, combineToolSources, type Identity } from "../src/index";
import { createContextTransform, type ContextProjection } from "../src/context";
import { createMcpToolSource } from "../src/mcp";

for (const mode of ["trim", "summarize"] as const) {
  test(`MCP with context ${mode}: paired structured result reaches model once and durable history stays intact`, async () => {
    const directory = mkdtempSync(join(tmpdir(), "lenso-mcp-context-"));
    const log = join(directory, "calls.txt");
    const identity: Identity = { subject: "alice", scope: "test", application: "joint", target: "local" };
    const source = createMcpToolSource<Identity>({
      application: "joint", instance: "local", sourceId: "joint-mcp",
      server: { command: process.execPath, args: [join(import.meta.dir, "fixtures/mcp-server.ts"), "normal", log] },
      selections: [{ name: "local_echo", tool: "echo", effect: "read" }],
      authorize: async (_selection, _input, context, actor) => context.subject === actor.subject,
      validateInput: input => { if (typeof input.value !== "string") throw Error("invalid"); },
    });
    const core = createFauxCore({ models: [{ id: "offline-joint" }], tokensPerSecond: Infinity });
    const reports: ContextProjection["report"][] = [];
    const store = createMemoryStore();
    const agent = createAgentService({
      store, identity: async (context: Identity) => context, model: core.getModel(),
      tools: combineToolSources([{ source, tools: source.tools }]),
      profiles: [{ id: "local", instructions: "Use only the authorized local source.", tools: source.tools }],
    }, { streamFn: core.streamSimple, transformContext: createContextTransform({
      mode, keepRecentMessages: 1,
      budget: { maxTokens: 5100, reserveTokens: 100, maxBytes: 5000,
        estimateTokens: messages => Buffer.byteLength(JSON.stringify(messages)) },
      summarize: async () => "Earlier completed assistant explanation of local data.",
    }, report => reports.push(report)) });
    try {
      const session = await agent.createSession(identity, "local");
      core.setResponses([fauxAssistantMessage("old detail ".repeat(600))]);
      await (await agent.startRun(identity, session.id, "Never deploy or spend money.")).done;
      const before = await agent.readSession(identity, session.id);
      core.setResponses([context => {
        const text = JSON.stringify(context.messages);
        expect(Buffer.byteLength(text)).toBeLessThanOrEqual(5000);
        expect(text).toContain("Never deploy or spend money.");
        expect(text).not.toContain("old detail ".repeat(600));
        return fauxAssistantMessage(fauxToolCall("local_echo", { value: "joint-result" }));
      }, context => {
        const result = context.messages.findLast(message => message.role === "toolResult");
        expect(result?.role).toBe("toolResult");
        if (result?.role !== "toolResult") throw Error("missing result");
        const text = result.content[0];
        expect(text?.type === "text" && JSON.parse(text.text).structuredContent.value).toBe("joint-result");
        expect(context.messages.some(message => message.role === "assistant" && message.content.some(part =>
          part.type === "toolCall" && part.id === result.toolCallId && part.name === result.toolName))).toBe(true);
        return fauxAssistantMessage("Completed locally.");
      }]);
      expect((await (await agent.startRun(identity, session.id, "Echo local data.")).done).status).toBe("completed");
      const after = await agent.readSession(identity, session.id);
      expect(after.messages.slice(0, before.messages.length)).toEqual(before.messages);
      expect(after.messages.some(message => JSON.stringify(message).includes("Historical summary"))).toBe(false);
      expect(after.calls).toHaveLength(1);
      expect(after.calls[0]?.status).toBe("completed");
      expect((after.calls[0]?.result as any).structuredContent.value).toBe("joint-result");
      expect(readFileSync(log, "utf8")).toBe("echo\n");
      expect(reports.some(report => report.mode === mode)).toBe(true);
    } finally {
      await agent.close(); await source.close(); store.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });
}
