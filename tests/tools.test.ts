import { expect, test } from "bun:test";
import { combineToolSources, toolFailure } from "../src/tools";
import { AgentError } from "../src/safety";
import type { Identity, ToolDefinition, ToolSource } from "../src/types";

const identity: Identity = { subject: "alice", scope: "studio", application: "studio", target: "local" };
const signal = new AbortController().signal;
function fixture(name: string) {
  let visible = true;
  const definition: ToolDefinition = {
    name, description: name, inputSchema: { type: "object" },
    target: { application: "studio", instance: "local", plugin: name, operation: "read" },
    effect: "read", confirmation: false,
  };
  const calls: unknown[][] = [];
  const source: ToolSource<Identity> = {
    async catalog(context) { return visible && context.subject === "alice" ? [definition] : []; },
    async prepare(name, input, context, actor, signal) {
      if (!visible) throw new AgentError("forbidden-operation");
      calls.push([name, input, context, actor, signal]);
      return { definition, input, precondition: null };
    },
    async invoke(prepared, context, actor, signal, confirmed) {
      if (!visible) throw new AgentError("forbidden-operation");
      calls.push([prepared, context, actor, signal, confirmed]);
      return { plugin: name };
    },
  };
  return { source, calls, definition, hide: () => { visible = false; } };
}

test("composition reserves hidden aliases, snapshots registration and rejects collisions before dispatch", () => {
  const f = fixture("studio_read");
  f.hide();
  expect(() => combineToolSources([
    { source: f.source, tools: ["studio_read"] }, { source: f.source, tools: ["studio_read"] },
  ])).toThrow("duplicate-tool");
  expect(() => combineToolSources([{ source: f.source, tools: ["bad.name"] }])).toThrow("invalid-tool-source");
  expect(f.calls).toHaveLength(0);
});

test("composition routes exact aliases and forwards admission data without caching caller visibility", async () => {
  const studio = fixture("studio_read"), relay = fixture("relay_read");
  const names = ["studio_read"];
  const combined = combineToolSources([
    { source: studio.source, tools: names }, { source: relay.source, tools: ["relay_read"] },
  ]);
  names.push("hidden");
  const catalog = await combined.catalog(identity, identity, signal);
  expect(catalog.map(tool => tool.name)).toEqual(["studio_read", "relay_read"]);
  catalog[0]!.description = "changed";
  expect(studio.definition.description).toBe("studio_read");
  expect(await combined.catalog({ ...identity, subject: "bob" }, identity, signal)).toEqual([]);
  const prepared = await combined.prepare("relay_read", { id: "7" }, identity, identity, signal);
  expect(await combined.invoke(prepared, identity, identity, signal, true)).toEqual({ plugin: "relay_read" });
  expect(relay.calls).toEqual([
    ["relay_read", { id: "7" }, identity, identity, signal],
    [prepared, identity, identity, signal, true],
  ]);
  expect(studio.calls).toHaveLength(0);
  relay.hide();
  expect((await combined.catalog(identity, identity, signal)).map(tool => tool.name)).toEqual(["studio_read"]);
  await expect(combined.invoke(prepared, identity, identity, signal, true)).rejects.toThrow("forbidden-operation");
  await expect(combined.prepare("hidden", {}, identity, identity, signal)).rejects.toThrow("unknown-operation");
});

test("composition rejects duplicate catalogs and substituted preparation names", async () => {
  const f = fixture("read");
  const combined = combineToolSources([{ source: f.source, tools: ["read"] }]);
  f.source.catalog = async () => [f.definition, f.definition];
  await expect(combined.catalog(identity, identity, signal)).rejects.toThrow("duplicate-tool");
  f.source.prepare = async (_name, input) => ({ definition: { ...f.definition, name: "other" }, input, precondition: null });
  await expect(combined.prepare("read", {}, identity, identity, signal)).rejects.toThrow("tool-mismatch");
});

test("recovery feedback never promotes arbitrary messages, payloads or codes to model instructions", () => {
  expect(toolFailure(new AgentError("invalid-input"))).toEqual({ error: { code: "invalid-input", recovery: "correct-input" } });
  expect(toolFailure(new AgentError("outcome-unknown"))).toEqual({ error: { code: "outcome-unknown", recovery: "inspect-outcome" } });
  for (const error of [new Error("private-secret"), new AgentError("private-secret"), { code: "__proto__" }])
    expect(toolFailure(error)).toEqual({ error: { code: "operation-failed", recovery: "stop" } });
});
