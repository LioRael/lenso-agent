import { Ajv, type ValidateFunction } from "ajv";
import type { ProviderDeclaration } from "@lenso/bun-plugin";
import type { InvocationContext } from "@lenso/contract-runtime";

import type { Infer, JsonSchema, Schema } from "./schema.js";

export type ToolExecution = "parallel_safe" | "exclusive";

export interface ExecuteResponse {
  readonly content_type: "text";
  readonly content: string;
  readonly content_blocks?: ReadonlyArray<unknown>;
  readonly metadata_json: string;
}

export type ExecuteError =
  | "invalid_arguments"
  | "permission_denied"
  | "not_found"
  | "output_limit_exceeded"
  | {
      readonly code: "execution_failed";
      readonly payload: {
        readonly reason_code: string;
        readonly message: string;
        readonly details_json: string;
      };
    };

export type ToolResult<Output> =
  | { readonly ok: true; readonly value: Output }
  | { readonly ok: false; readonly error: ExecuteError }
  | ExecuteResponse;

export interface ToolOptions<Input, Output> {
  readonly name: string;
  readonly description?: string;
  readonly input: Schema<Input>;
  readonly output: Schema<Output>;
  readonly execution?: ToolExecution;
}

export type ToolHandler<Input, Output, Instance extends object> = (
  input: Input,
  call: InvocationContext,
  instance: Instance,
) => ToolResult<Output> | Promise<ToolResult<Output>>;

export interface ToolDeclaration<Input, Output, Instance extends object> {
  readonly kind: "lenso.agent.tool";
  readonly options: ToolOptions<Input, Output>;
  readonly handler: ToolHandler<Input, Output, Instance>;
}

type ToolForInstance<Instance extends object> = {
  readonly kind: "lenso.agent.tool";
  readonly options: ToolOptions<unknown, unknown>;
  readonly handler: (
    input: never,
    call: InvocationContext,
    instance: Instance,
  ) => unknown;
};

export function tool<
  InputSchema extends Schema<unknown>,
  OutputSchema extends Schema<unknown>,
  Instance extends object = object,
>(
  options: Omit<ToolOptions<Infer<InputSchema>, Infer<OutputSchema>>, "input" | "output"> & {
    readonly input: InputSchema;
    readonly output: OutputSchema;
  },
  handler: ToolHandler<Infer<InputSchema>, Infer<OutputSchema>, Instance>,
): ToolDeclaration<Infer<InputSchema>, Infer<OutputSchema>, Instance> {
  const declaration: ToolDeclaration<
    Infer<InputSchema>,
    Infer<OutputSchema>,
    Instance
  > = {
    kind: "lenso.agent.tool" as const,
    options: options as ToolOptions<Infer<InputSchema>, Infer<OutputSchema>>,
    handler,
  };
  return Object.freeze(declaration);
}

const descriptor = Object.freeze({
  capability_id: "lenso.agent.tool-provider@2",
  descriptor_version: "2.1.0",
  descriptor_digest:
    "sha256:8bfc7951a77a853b22d6a1a03d31d36a11844ba5d3526fec0934bf95977ad80d",
  operations: Object.freeze(["catalog", "execute"]),
  stream_operations: Object.freeze([]),
  event_operations: Object.freeze([]),
});

/** Declares one ToolProvider backed by the admitted Plugin instance. */
export function tools<Instance extends object>(
  declarations: ReadonlyArray<ToolForInstance<Instance>>,
): ProviderDeclaration<Instance> {
  if (declarations.length === 0) throw new Error("tools requires at least one tool");
  const names = new Set<string>();
  for (const declaration of declarations) {
    if (names.has(declaration.options.name)) {
      throw new Error(`duplicate Tool name ${declaration.options.name}`);
    }
    names.add(declaration.options.name);
  }
  return Object.freeze({
    kind: "lenso.provider" as const,
    descriptor,
    bind() {
      throw new Error("tools declarations must be compiled by the Lenso build frontend");
    },
  });
}

const ajv = new Ajv({ allErrors: true, strict: true });
const validators = new Map<string, ValidateFunction>();

/** Runtime support used by generated Agent ToolProvider binders. */
export function validateToolValue(schema: JsonSchema, value: unknown): boolean {
  const key = JSON.stringify(schema);
  const existing = validators.get(key);
  if (existing !== undefined) return existing(value) as boolean;
  const validate = ajv.compile(schema);
  validators.set(key, validate);
  return validate(value) as boolean;
}

/** Identifies the existing explicit ToolProvider response escape hatch. */
export function isExecuteResponse(value: unknown): value is ExecuteResponse {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const response = value as Readonly<Record<string, unknown>>;
  if (
    response.content_type === "text" &&
    typeof response.content === "string" &&
    response.content.length <= 1_048_576 &&
    typeof response.metadata_json === "string" &&
    response.metadata_json.length >= 2 &&
    response.metadata_json.length <= 65_536
  ) {
    try {
      JSON.parse(response.metadata_json);
      return true;
    } catch {
      return false;
    }
  }
  return false;
}
