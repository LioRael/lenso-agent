import { createHash, randomUUID } from "node:crypto";
import { definePlugin, type Plugin, type PluginContext } from "@lenso/core";
import { stableJson } from "@lenso/engine/diagnostics";
import { AgentError, jsonCopy } from "./safety";
import { createMcpStdioClient, type McpStdioOptions, type McpTool } from "./mcp-client";
import type { Identity, PreparedTool, ToolDefinition, ToolSource } from "./types";

export { createMcpStdioClient } from "./mcp-client";
export type { McpStdioClient, McpStdioOptions, McpTool, McpToolResult, McpCatalog } from "./mcp-client";

export interface McpToolSelection {
  /** Stable public alias reserved for this source, including when unavailable. */
  readonly name: string;
  readonly tool: string;
  /** Trusted host policy, never inferred from server annotations. Defaults to unknown. */
  readonly effect?: ToolDefinition["effect"];
  readonly confirmation?: boolean;
}
export interface McpToolSourceOptions<C> {
  readonly application: string;
  readonly instance: string;
  /** Unique stable source identity within this host; becomes target.plugin. */
  readonly sourceId: string;
  readonly server: McpStdioOptions;
  readonly selections: readonly McpToolSelection[];
  /** Undefined input checks visibility. Rechecked at preparation and invocation. */
  readonly authorize: (selection: McpToolSelection, input: unknown, context: C, identity: Identity) => Promise<boolean>;
  /** Validate raw JSON against the current schema; throw on rejection. No transformation. */
  readonly validateInput: (input: Record<string, unknown>, schema: Record<string, unknown>) => void | Promise<void>;
}
export interface McpToolSource<C> extends ToolSource<C> {
  readonly tools: readonly string[];
  close(): Promise<void>;
}

export function createMcpToolSource<C>(options: McpToolSourceOptions<C>): McpToolSource<C> {
  const { application, instance, sourceId, authorize, validateInput } = options;
  if (![application, instance, sourceId].every(value => typeof value === "string" && value.length > 0) ||
      typeof authorize !== "function" || typeof validateInput !== "function") throw new AgentError("invalid-tool-source");
  const selections = options.selections.map(entry => Object.freeze({ ...entry }));
  const byName = new Map<string, McpToolSelection>();
  for (const entry of selections) {
    if (!/^[A-Za-z][A-Za-z0-9_-]{0,127}$/.test(entry.name) || byName.has(entry.name) ||
        typeof entry.tool !== "string" || !entry.tool ||
        (entry.effect !== undefined && !["read", "write", "unknown"].includes(entry.effect)) ||
        (entry.confirmation !== undefined && typeof entry.confirmation !== "boolean")) throw new AgentError("invalid-tool-source");
    byName.set(entry.name, entry);
  }
  const client = createMcpStdioClient(options.server), incarnation = randomUUID();
  function check(identity: Identity, signal: AbortSignal) {
    if (signal.aborted) throw new AgentError("cancelled");
    if (identity.application !== application || identity.target !== instance) throw new AgentError("forbidden-operation");
  }
  function definition(entry: McpToolSelection, tool: McpTool): ToolDefinition {
    const effect = entry.effect ?? "unknown";
    return jsonCopy({
      name: entry.name, description: tool.description ?? tool.name, inputSchema: tool.inputSchema,
      target: { application, instance, plugin: sourceId, operation: entry.tool },
      effect, confirmation: effect !== "read" || entry.confirmation === true,
    });
  }
  async function prepare(name: string, input: unknown, context: C, identity: Identity, signal: AbortSignal) {
    check(identity, signal);
    const entry = byName.get(name);
    if (!entry) throw new AgentError("unknown-operation");
    const visible = await authorize(entry, undefined, context, identity);
    check(identity, signal);
    if (visible !== true) throw new AgentError("forbidden-operation");
    const catalog = await client.listTools(signal), tool = catalog.tools.find(tool => tool.name === entry.tool);
    if (!tool) throw new AgentError("unknown-operation");
    const raw = jsonCopy(input);
    if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new AgentError("invalid-input");
    try { await validateInput(jsonCopy(raw) as Record<string, unknown>, jsonCopy(tool.inputSchema)); }
    catch { check(identity, signal); throw new AgentError("invalid-input"); }
    check(identity, signal);
    const allowed = await authorize(entry, jsonCopy(raw), context, identity);
    check(identity, signal);
    if (allowed !== true) throw new AgentError("forbidden-operation");
    const projected = definition(entry, tool);
    const precondition = createHash("sha256").update(stableJson({
      incarnation, revision: catalog.revision, definition: projected, outputSchema: tool.outputSchema ?? null,
    })).digest("hex");
    return { prepared: { definition: projected, input: raw, precondition } satisfies PreparedTool, revision: catalog.revision };
  }
  return {
    tools: Object.freeze(selections.map(entry => entry.name)),
    async catalog(context, identity, signal) {
      check(identity, signal);
      const visible: McpToolSelection[] = [];
      for (const entry of selections) {
        if (await authorize(entry, undefined, context, identity) === true) visible.push(entry);
        check(identity, signal);
      }
      if (!visible.length) return [];
      const catalog = await client.listTools(signal);
      const tools = new Map(catalog.tools.map(tool => [tool.name, tool]));
      return visible.flatMap(entry => tools.has(entry.tool) ? [definition(entry, tools.get(entry.tool)!)] : []);
    },
    async prepare(name, input, context, identity, signal) {
      return (await prepare(name, input, context, identity, signal)).prepared;
    },
    async invoke(prepared, context, identity, signal, confirmed) {
      const admitted = jsonCopy(prepared);
      const current = await prepare(admitted.definition.name, admitted.input, context, identity, signal);
      if (stableJson(current.prepared) !== stableJson(admitted)) throw new AgentError("tool-conflict");
      if (current.prepared.definition.confirmation && confirmed !== true) throw new AgentError("confirmation-required");
      return client.callTool(byName.get(admitted.definition.name)!.tool,
        current.prepared.input as Record<string, unknown>, signal, current.revision);
    },
    close: () => client.close(),
  };
}

export interface McpToolPluginOptions<C> {
  id: string;
  requires?: readonly Plugin<unknown>[];
  options(context: PluginContext): McpToolSourceOptions<C> | Promise<McpToolSourceOptions<C>>;
}
/** Cleanup belongs to the optional plugin; agent provider and lifecycle stay unchanged. */
export function createMcpToolPlugin<C>(options: McpToolPluginOptions<C>): Plugin<McpToolSource<C>> {
  return definePlugin({
    id: options.id, requires: options.requires ?? [],
    async setup(context) {
      const source = createMcpToolSource(await options.options(context));
      context.onCleanup(() => source.close());
      return source;
    },
  });
}
