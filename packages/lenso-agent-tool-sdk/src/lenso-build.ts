import type {
  BuildArgument,
  BuildValue,
  DeclarationArgument,
  LoweringInput,
  LoweringOutput,
  SourceSpan,
} from "@lenso/bun-plugin/build";

const CAPABILITY_ID = "lenso.agent.tool-provider@2";
const DESCRIPTOR_VERSION = "2.1.0";
const DESCRIPTOR_DIGEST =
  "sha256:8bfc7951a77a853b22d6a1a03d31d36a11844ba5d3526fec0934bf95977ad80d";
const GENERATED_MODULE = "generated/agent-tool-provider.ts";
const GENERATED_EXPORT = "bindAgentTools";

interface LoweredTool {
  readonly name: string;
  readonly description: string;
  readonly execution: "parallel_safe" | "exclusive";
  readonly input: Readonly<Record<string, unknown>>;
  readonly output: Readonly<Record<string, unknown>>;
  readonly handler: string;
}

interface CompiledSchema {
  readonly schema: Readonly<Record<string, unknown>>;
  readonly optional: boolean;
}

/** Lowers Agent-owned tools/tool syntax into one ordinary ToolProvider binder. */
export function lower(input: LoweringInput): LoweringOutput {
  if (input.api_version !== 1) fail(input.span, `unsupported build API ${input.api_version}`);
  if (input.export_name !== "tools") {
    fail(input.span, `Agent Tool lowering expects tools, received ${input.export_name}`);
  }
  exactArguments(input.arguments, 1, "tools", input.span);
  const declarations = arrayValue(input.arguments[0]!, "tools argument", input.span);
  if (declarations.length === 0) fail(input.span, "tools requires at least one tool");
  if (declarations.length > 256) fail(input.span, "tools exceeds the catalog limit of 256");

  const tools = declarations.map((argument, index) =>
    compileTool(asArgument(argument, `tools[${index}]`, input.span), input, index),
  );
  const names = new Set<string>();
  for (const tool of tools) {
    if (names.has(tool.name)) fail(input.span, `duplicate Tool name ${tool.name}`);
    names.add(tool.name);
  }

  return Object.freeze({
    api_version: 1 as const,
    providers: Object.freeze([
      Object.freeze({
        capability_id: CAPABILITY_ID,
        descriptor_version: DESCRIPTOR_VERSION,
        descriptor_digest: DESCRIPTOR_DIGEST,
        binder: Object.freeze({
          module: GENERATED_MODULE,
          export_name: GENERATED_EXPORT,
        }),
        handler_references: Object.freeze(tools.map((tool) => tool.handler)),
      }),
    ]),
    files: Object.freeze([
      Object.freeze({
        path: GENERATED_MODULE,
        contents: generatedBinder(tools),
      }),
    ]),
    diagnostics: Object.freeze([]),
  });
}

function compileTool(
  argument: BuildArgument,
  root: LoweringInput,
  index: number,
): LoweredTool {
  const declaration = requiredDeclaration(argument, `tools[${index}]`, root.span);
  samePackage(declaration, root, "tool");
  exactArguments(declaration.arguments, 2, "tool", declaration.span);
  const options = objectValue(declaration.arguments[0]!, "tool options", declaration.span);
  exactKeys(
    options,
    ["name", "description", "input", "output", "execution"],
    ["name", "input", "output"],
    "tool options",
    declaration.span,
  );
  const handler = declaration.arguments[1]!;
  if (handler.kind !== "handler") fail(handler.span, "tool handler must be a source function");
  const name = stringProperty(options, "name", declaration.span);
  if (name.length === 0 || name.length > 128) {
    fail(declaration.span, "Tool name must contain 1 to 128 characters");
  }
  const description = optionalStringProperty(options, "description", declaration.span) ?? "";
  if (description.length > 4096) fail(declaration.span, "Tool description exceeds 4096 characters");
  const execution = optionalStringProperty(options, "execution", declaration.span) ?? "exclusive";
  if (execution !== "parallel_safe" && execution !== "exclusive") {
    fail(declaration.span, "Tool execution must be parallel_safe or exclusive");
  }
  const input = compileSchema(propertyArgument(options, "input", declaration.span), root).schema;
  const output = compileSchema(propertyArgument(options, "output", declaration.span), root).schema;
  const inputJson = JSON.stringify(input);
  if (inputJson.length < 2 || inputJson.length > 65_536) {
    fail(declaration.span, "Tool input schema must encode to 2 through 65536 characters");
  }
  return Object.freeze({ name, description, execution, input, output, handler: handler.reference });
}

function compileSchema(argument: BuildArgument, root: LoweringInput): CompiledSchema {
  if (argument.kind === "value") {
    const value = argument.value;
    if (isRecord(value) && "jsonSchema" in value) {
      const schemaArgument = asArgument(value.jsonSchema, "schema.jsonSchema", argument.span);
      return { schema: plainJsonObject(schemaArgument, "schema.jsonSchema"), optional: false };
    }
    return { schema: plainJsonObject(argument, "schema"), optional: false };
  }
  const declaration = requiredDeclaration(argument, "schema", root.span);
  if (declaration.package.name !== root.package.name ||
      declaration.package.version !== root.package.version ||
      declaration.package.integrity !== root.package.integrity) {
    fail(declaration.span, "schema declaration must come from the same locked Agent SDK");
  }
  switch (declaration.export_name) {
    case "string":
    case "number":
    case "boolean":
      exactArguments(declaration.arguments, 0, declaration.export_name, declaration.span);
      return { schema: { type: declaration.export_name }, optional: false };
    case "unknown":
      exactArguments(declaration.arguments, 0, "unknown", declaration.span);
      return { schema: {}, optional: false };
    case "literal": {
      exactArguments(declaration.arguments, 1, "literal", declaration.span);
      return { schema: { const: primitiveValue(declaration.arguments[0]!, "literal") }, optional: false };
    }
    case "array": {
      exactArguments(declaration.arguments, 1, "array", declaration.span);
      return { schema: { type: "array", items: compileSchema(declaration.arguments[0]!, root).schema }, optional: false };
    }
    case "optional": {
      exactArguments(declaration.arguments, 1, "optional", declaration.span);
      return { schema: compileSchema(declaration.arguments[0]!, root).schema, optional: true };
    }
    case "object": {
      exactArguments(declaration.arguments, 1, "object", declaration.span);
      const shape = objectValue(declaration.arguments[0]!, "object shape", declaration.span);
      const properties: Record<string, Readonly<Record<string, unknown>>> = {};
      const required: string[] = [];
      for (const [name, value] of Object.entries(shape)) {
        const child = compileSchema(asArgument(value, `object property ${name}`, declaration.span), root);
        properties[name] = child.schema;
        if (!child.optional) required.push(name);
      }
      return {
        schema: { type: "object", properties, required, additionalProperties: false },
        optional: false,
      };
    }
    default:
      fail(declaration.span, `unsupported Agent schema declaration ${declaration.export_name}`);
  }
}

function generatedBinder(tools: ReadonlyArray<LoweredTool>): string {
  const definitions = JSON.stringify(tools);
  return `import type { CapabilityProviderBinding, InvocationContext, ProviderDispatchOutcome } from "@lenso/bun-plugin";
import { isExecuteResponse, validateToolValue, type ExecuteError, type ToolResult } from "@lenso/agent-tool-sdk";
import { resolveHandler } from "lenso:build-handlers";

const definitions = ${definitions} as const;
const handlers = definitions.map((definition) => resolveHandler(definition.handler));

export function bindAgentTools(instance: object): CapabilityProviderBinding {
  return {
    descriptor: {
      capability_id: ${JSON.stringify(CAPABILITY_ID)},
      descriptor_version: ${JSON.stringify(DESCRIPTOR_VERSION)},
      descriptor_digest: ${JSON.stringify(DESCRIPTOR_DIGEST)},
      operations: ["catalog", "execute"],
      stream_operations: [],
      event_operations: [],
    },
    async invokeRequest(operation: string, call: InvocationContext, payload: unknown): Promise<ProviderDispatchOutcome> {
      if (operation === "catalog") {
        if (!isRecord(payload) || Object.keys(payload).length !== 0) return { kind: "domain", value: "catalog_invalid" };
        return {
          kind: "success",
          value: {
            tools: definitions.map(({ name, description, execution, input }) => ({
              name,
              description,
              execution,
              input_schema_json: JSON.stringify(input),
            })),
          },
        };
      }
      if (operation !== "execute") return { kind: "runtime", failure: { kind: "unknown_operation" } };
      if (!isRecord(payload) || typeof payload.name !== "string" || typeof payload.arguments_json !== "string") {
        return { kind: "domain", value: "invalid_arguments" };
      }
      const index = definitions.findIndex((definition) => definition.name === payload.name);
      if (index < 0) return { kind: "domain", value: "not_found" };
      const definition = definitions[index]!;
      let input: unknown;
      try { input = JSON.parse(payload.arguments_json); } catch { return { kind: "domain", value: "invalid_arguments" }; }
      if (!validateToolValue(definition.input, input)) return { kind: "domain", value: "invalid_arguments" };
      const result = await handlers[index]!(input, call, instance) as ToolResult<unknown>;
      if (isExecuteResponse(result)) return { kind: "success", value: result };
      if (!isRecord(result) || typeof result.ok !== "boolean") return pluginFailure("Tool handler returned an invalid result");
      if (!result.ok) {
        if (!isExecuteError(result.error)) return pluginFailure("Tool handler returned an invalid ExecuteError");
        return { kind: "domain", value: result.error };
      }
      if (!validateToolValue(definition.output, result.value)) return pluginFailure("Tool handler output failed its declared schema");
      let content: string | undefined;
      try { content = JSON.stringify(result.value); } catch { return pluginFailure("Tool handler output is not JSON encodable"); }
      if (content === undefined) return pluginFailure("Tool handler output is not JSON encodable");
      if (content.length > 1_048_576) return { kind: "domain", value: "output_limit_exceeded" };
      return { kind: "success", value: { content_type: "text", content, metadata_json: "{}" } };
    },
  };
}

function pluginFailure(detail: string): ProviderDispatchOutcome {
  return { kind: "runtime", failure: { kind: "plugin_failure", detail } };
}

function isExecuteError(value: unknown): value is ExecuteError {
  if (value === "invalid_arguments" || value === "permission_denied" || value === "not_found" || value === "output_limit_exceeded") return true;
  if (!isRecord(value) || value.code !== "execution_failed" || !isRecord(value.payload)) return false;
  if (typeof value.payload.reason_code !== "string" || value.payload.reason_code.length < 1 || value.payload.reason_code.length > 128) return false;
  if (typeof value.payload.message !== "string" || value.payload.message.length > 4096) return false;
  if (typeof value.payload.details_json !== "string" || value.payload.details_json.length < 2 || value.payload.details_json.length > 65536) return false;
  try { JSON.parse(value.payload.details_json); return true; } catch { return false; }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
`;
}

function samePackage(declaration: DeclarationArgument, root: LoweringInput, exportName: string): void {
  if (declaration.export_name !== exportName || declaration.package.name !== root.package.name ||
      declaration.package.version !== root.package.version || declaration.package.integrity !== root.package.integrity) {
    fail(declaration.span, `${exportName} declaration must come from the same locked Agent SDK`);
  }
}

function requiredDeclaration(argument: BuildArgument, subject: string, span: SourceSpan): DeclarationArgument {
  if (argument.kind !== "declaration") fail(span, `${subject} must be a declaration`);
  return argument;
}

function exactArguments(arguments_: ReadonlyArray<BuildArgument>, count: number, subject: string, span: SourceSpan): void {
  if (arguments_.length !== count) fail(span, `${subject} expects ${count} argument${count === 1 ? "" : "s"}`);
}

function arrayValue(argument: BuildArgument, subject: string, span: SourceSpan): ReadonlyArray<BuildValue | BuildArgument> {
  if (argument.kind !== "value" || !Array.isArray(argument.value)) fail(span, `${subject} must be a static array`);
  return argument.value;
}

function objectValue(argument: BuildArgument, subject: string, span: SourceSpan): Readonly<Record<string, BuildValue | BuildArgument>> {
  if (argument.kind !== "value" || !isRecord(argument.value)) fail(span, `${subject} must be a static object`);
  return argument.value as Readonly<Record<string, BuildValue | BuildArgument>>;
}

function plainJsonObject(argument: BuildArgument, subject: string): Readonly<Record<string, unknown>> {
  if (argument.kind !== "value" || !isRecord(argument.value)) fail(argument.span, `${subject} must be a JSON object`);
  const convert = (value: BuildValue | BuildArgument): unknown => {
    if (isArgument(value)) {
      if (value.kind !== "value") fail(value.span, `${subject} must contain only JSON values`);
      return convert(value.value);
    }
    if (Array.isArray(value)) return value.map(convert);
    if (isRecord(value)) return Object.fromEntries(Object.entries(value).map(([key, child]) => [key, convert(child as BuildValue | BuildArgument)]));
    return value;
  };
  return convert(argument) as Readonly<Record<string, unknown>>;
}

function primitiveValue(argument: BuildArgument, subject: string): string | number | boolean | null {
  if (argument.kind !== "value" || (typeof argument.value === "object" && argument.value !== null)) {
    fail(argument.span, `${subject} must be a JSON primitive`);
  }
  return argument.value as string | number | boolean | null;
}

function propertyArgument(object: Readonly<Record<string, BuildValue | BuildArgument>>, key: string, span: SourceSpan): BuildArgument {
  return asArgument(object[key]!, key, span);
}

function stringProperty(object: Readonly<Record<string, BuildValue | BuildArgument>>, key: string, span: SourceSpan): string {
  const argument = propertyArgument(object, key, span);
  if (argument.kind !== "value" || typeof argument.value !== "string") fail(argument.span, `${key} must be a string`);
  return argument.value;
}

function optionalStringProperty(object: Readonly<Record<string, BuildValue | BuildArgument>>, key: string, span: SourceSpan): string | undefined {
  return object[key] === undefined ? undefined : stringProperty(object, key, span);
}

function exactKeys(
  object: Readonly<Record<string, unknown>>,
  allowed: ReadonlyArray<string>,
  required: ReadonlyArray<string>,
  subject: string,
  span: SourceSpan,
): void {
  const unknown = Object.keys(object).find((key) => !allowed.includes(key));
  if (unknown !== undefined) fail(span, `${subject} contains unknown field ${unknown}`);
  const missing = required.find((key) => !(key in object));
  if (missing !== undefined) fail(span, `${subject} is missing field ${missing}`);
}

function asArgument(value: BuildValue | BuildArgument, subject: string, span: SourceSpan): BuildArgument {
  if (isArgument(value)) return value;
  return { kind: "value", value, span };
}

function isArgument(value: unknown): value is BuildArgument {
  return isRecord(value) && typeof value.kind === "string" && "span" in value;
}

function isRecord(value: unknown): value is Readonly<Record<string, unknown>> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function fail(span: SourceSpan, message: string): never {
  const error = new Error(message) as Error & { span: SourceSpan };
  error.name = "AgentToolLoweringError";
  error.span = span;
  throw error;
}
