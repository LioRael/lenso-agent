import { AgentError, jsonCopy, safeCode } from "./safety";
import type { ToolDefinition, ToolSource } from "./types";

export interface ToolSourceRegistration<C> {
  readonly source: ToolSource<C>;
  /** Stable public aliases owned by this source, including currently hidden tools. */
  readonly tools: readonly string[];
}

/** Compose plugin sources without caching a caller's catalog or weakening admission. */
export function combineToolSources<C>(registrations: readonly ToolSourceRegistration<C>[]): ToolSource<C> {
  const owners = new Map<string, ToolSource<C>>();
  const entries = registrations.map(({ source, tools }) => {
    if (!source || [source.catalog, source.prepare, source.invoke].some(value => typeof value !== "function"))
      throw new AgentError("invalid-tool-source");
    const names = new Set<string>();
    for (const name of tools) {
      if (!/^[A-Za-z][A-Za-z0-9_-]{0,127}$/.test(name)) throw new AgentError("invalid-tool-source");
      if (owners.has(name)) throw new AgentError("duplicate-tool");
      owners.set(name, source);
      names.add(name);
    }
    return { source, names };
  });
  function owner(name: string): ToolSource<C> {
    const source = owners.get(name);
    if (!source) throw new AgentError("unknown-operation");
    return source;
  }
  return {
    async catalog(context, identity, signal) {
      const result: ToolDefinition[] = [];
      const seen = new Set<string>();
      for (const { source, names } of entries) {
        signal.throwIfAborted();
        for (const definition of await source.catalog(context, identity, signal)) {
          if (!names.has(definition.name)) continue;
          if (seen.has(definition.name)) throw new AgentError("duplicate-tool");
          seen.add(definition.name);
          result.push(jsonCopy(definition));
        }
      }
      signal.throwIfAborted();
      return result;
    },
    async prepare(name, input, context, identity, signal) {
      signal.throwIfAborted();
      const prepared = await owner(name).prepare(name, input, context, identity, signal);
      if (prepared.definition.name !== name) throw new AgentError("tool-mismatch");
      return prepared;
    },
    invoke(prepared, context, identity, signal, confirmed) {
      signal.throwIfAborted();
      return owner(prepared.definition.name).invoke(prepared, context, identity, signal, confirmed);
    },
  };
}

export interface ToolFailure {
  error: {
    code: string;
    recovery: "correct-input" | "request-authorization" | "refresh-state" | "inspect-outcome" | "stop";
  };
}

/** Fixed vocabulary only: never expose arbitrary exception messages or diagnostic payloads. */
export function toolFailure(error: unknown): ToolFailure {
  const recovery = {
    "invalid-input": "correct-input",
    "forbidden-operation": "request-authorization",
    "confirmation-required": "request-authorization",
    "approval-required": "request-authorization",
    "precondition-conflict": "refresh-state",
    "tool-conflict": "refresh-state",
    "unknown-operation": "refresh-state",
    "outcome-unknown": "inspect-outcome",
    "cancelled": "stop",
    "output-budget": "stop",
    "operation-failed": "stop",
  } as const;
  const candidate = safeCode(error);
  const code = Object.hasOwn(recovery, candidate) ? candidate as keyof typeof recovery : "operation-failed";
  return { error: { code, recovery: recovery[code] } };
}
