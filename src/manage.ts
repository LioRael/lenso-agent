import type { Plugin } from "@lenso/core";
import {
  boundedJson,
  validateOperationInput,
  validateOperations,
  type Operation,
  type OperationInvocationOptions,
  type OperationRuntime,
} from "@lenso/engine/operations";
import { createManageAdapter } from "@lenso/manage";
import { createAgentTools } from "@lenso/manage/agent";
import { AgentError, jsonCopy, safeCode } from "./safety";
import type { Identity, PreparedTool, ToolDefinition, ToolSource } from "./types";

export interface ManageToolSelection {
  readonly name: string;
  readonly operation: Operation;
  readonly confirmation?: boolean;
}

export interface ManageToolSourceOptions<C> {
  readonly running: OperationRuntime;
  readonly application: string;
  readonly plugins: readonly Plugin<unknown>[];
  readonly selections: readonly ManageToolSelection[];
  /** Undefined input checks visibility; invocation checks receive schema-validated input. */
  readonly authorize: (
    operation: Operation, input: unknown | undefined, context: C, identity: Identity,
  ) => Promise<boolean>;
  /** Derive the business actor from identity and place signal in cooperative business context. */
  readonly binding: (
    operation: Operation, validatedInput: unknown, context: C,
    identity: Identity, signal: AbortSignal,
  ) => OperationInvocationOptions | Promise<OperationInvocationOptions>;
  /** Advisory snapshot only; the business service must enforce version CAS at the effect. */
  readonly precondition?: (
    operation: Operation, validatedInput: unknown, context: C, identity: Identity,
  ) => Promise<string | null>;
}

export function createManageToolSource<C>(options: ManageToolSourceOptions<C>): ToolSource<C> {
  const { running, application, authorize, binding, precondition } = options;
  const plugins = [...options.plugins];
  const selection = options.selections.map((entry) => ({ ...entry }));
  const names = new Set<string>();
  if (typeof application !== "string" || !application ||
      typeof authorize !== "function" || typeof binding !== "function" ||
      (precondition !== undefined && typeof precondition !== "function"))
    throw new AgentError("invalid-tool-source");
  for (const entry of selection) {
    if (!/^[A-Za-z][A-Za-z0-9_-]{0,127}$/.test(entry.name) ||
        /^operation_[0-9]+$/.test(entry.name) || names.has(entry.name) ||
        (entry.confirmation !== undefined && typeof entry.confirmation !== "boolean"))
      throw new AgentError("invalid-tool-source");
    names.add(entry.name);
  }
  // Validate the whole selection, including exact running plugin objects and duplicate methods.
  createManageAdapter({
    running, plugins,
    operations: validateOperations(plugins, selection.map((entry) => entry.operation)),
    canList: async () => false,
    binding: async () => { throw new AgentError("operation-failed"); },
  });
  const byName = new Map(selection.map((entry) => [entry.name, entry]));

  async function safe<T>(signal: AbortSignal, work: () => Promise<T>): Promise<T> {
    try {
      if (signal.aborted) throw new AgentError("cancelled");
      const result = await work();
      if (signal.aborted) throw new AgentError("cancelled");
      return result;
    } catch (error) {
      if (signal.aborted) throw new AgentError("cancelled");
      let cause = error;
      for (let depth = 0; cause && typeof cause === "object" && depth < 8; depth++) {
        if (cause instanceof AgentError) throw cause;
        cause = Reflect.get(cause, "cause");
      }
      const diagnostic = error && typeof error === "object" ? Reflect.get(error, "diagnostic") : undefined;
      throw new AgentError(safeCode(diagnostic ?? error));
    }
  }

  function checkIdentity(identity: Identity): void {
    if (identity.application !== application || identity.target !== running.instanceId)
      throw new AgentError("forbidden-operation");
  }

  async function authorized(operation: Operation, input: unknown, context: C, identity: Identity): Promise<void> {
    if (await authorize(operation, input, context, identity) !== true)
      throw new AgentError("forbidden-operation");
  }

  async function token(operation: Operation, input: unknown, context: C, identity: Identity): Promise<string | null> {
    const value = precondition ? await precondition(operation, input, context, identity) : null;
    if (value !== null && typeof value !== "string") throw new AgentError("invalid-precondition");
    boundedJson(value);
    return value;
  }

  async function project(
    entry: ManageToolSelection, context: C, identity: Identity, signal: AbortSignal,
    admitted?: { prepared: PreparedTool; confirmed: boolean },
  ) {
    const adapter = createManageAdapter({
      running, plugins, operations: [entry.operation],
      canList: async (operation) => {
        if (signal.aborted) throw new AgentError("cancelled");
        return await authorize(operation, undefined, context, identity) === true;
      },
      binding: async (operation, validatedInput) => {
        if (!admitted) throw new AgentError("operation-failed");
        await authorized(operation, validatedInput, context, identity);
        if (await token(operation, validatedInput, context, identity) !== admitted.prepared.precondition)
          throw new AgentError("precondition-conflict");
        const trusted = await binding(operation, validatedInput, context, identity, signal);
        if (!trusted || typeof trusted !== "object" || Array.isArray(trusted))
          throw new AgentError("invalid-tool-binding");
        return { ...trusted, signal, confirm: () => admitted.confirmed === true };
      },
    });
    // The adapter's entry key is ephemeral. Only the host's semantic alias leaves this source.
    const [tool] = await createAgentTools(adapter);
    if (!tool) throw new AgentError("forbidden-operation");
    const definition: ToolDefinition = jsonCopy({
      name: entry.name,
      description: tool.description,
      inputSchema: tool.inputSchema,
      target: {
        application, instance: running.instanceId,
        plugin: entry.operation.plugin.id, operation: entry.operation.method,
      },
      effect: entry.operation.effect ?? "unknown",
      confirmation: entry.operation.confirmation === "required" ||
        (entry.confirmation ?? entry.operation.effect !== "read"),
    });
    return { definition, tool };
  }

  async function prepare(
    name: string, input: unknown, context: C, identity: Identity, signal: AbortSignal,
  ): Promise<PreparedTool> {
    return safe(signal, async () => {
      checkIdentity(identity);
      const entry = byName.get(name);
      if (!entry) throw new AgentError("unknown-operation");
      const raw = jsonCopy(input);
      const validated = await validateOperationInput(entry.operation, jsonCopy(raw));
      await authorized(entry.operation, validated, context, identity);
      const currentToken = await token(entry.operation, validated, context, identity);
      const { definition } = await project(entry, context, identity, signal);
      // Retain raw JSON: Manage validates it again; transformed output is not valid raw input.
      return { definition, input: raw, precondition: currentToken };
    });
  }

  return {
    catalog: (context, identity, signal) => safe(signal, async () => {
      checkIdentity(identity);
      const result: ToolDefinition[] = [];
      for (const entry of selection) {
        try {
          result.push((await project(entry, context, identity, signal)).definition);
        } catch (error) {
          if (!(error instanceof AgentError) || error.code !== "forbidden-operation") throw error;
        }
      }
      return jsonCopy(result);
    }),
    prepare,
    invoke: (prepared, context, identity, signal, confirmed) => safe(signal, async () => {
      const admitted = jsonCopy(prepared);
      const current = await prepare(admitted.definition.name, admitted.input, context, identity, signal);
      if (current.precondition !== admitted.precondition)
        throw new AgentError("precondition-conflict");
      if (boundedJson(current) !== boundedJson(admitted))
        throw new AgentError("tool-conflict");
      if (current.definition.confirmation && confirmed !== true)
        throw new AgentError("confirmation-required");
      const entry = byName.get(current.definition.name)!;
      const selected = await project(entry, context, identity, signal, {
        prepared: current, confirmed,
      });
      if (boundedJson(selected.definition) !== boundedJson(current.definition))
        throw new AgentError("tool-conflict");
      return selected.tool.invoke(current.input);
    }),
  };
}
