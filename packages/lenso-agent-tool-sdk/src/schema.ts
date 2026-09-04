export type JsonSchema = Readonly<Record<string, unknown>>;

declare const schemaValue: unique symbol;

/** A JSON Schema carrying its decoded TypeScript value type. */
export interface Schema<Value> {
  readonly jsonSchema: JsonSchema;
  readonly [schemaValue]?: Value;
}

export type Infer<Declaration> = Declaration extends Schema<infer Value>
  ? Value
  : never;

export function string(): Schema<string> {
  return frozen({ type: "string" });
}

export function number(): Schema<number> {
  return frozen({ type: "number" });
}

export function boolean(): Schema<boolean> {
  return frozen({ type: "boolean" });
}

export function unknown(): Schema<unknown> {
  return frozen({});
}

export function literal<const Value extends string | number | boolean | null>(
  value: Value,
): Schema<Value> {
  return frozen({ const: value });
}

export function array<Item>(items: Schema<Item>): Schema<ReadonlyArray<Item>> {
  return frozen({ type: "array", items: items.jsonSchema });
}

export interface OptionalSchema<Value> extends Schema<Value | undefined> {
  readonly optional: true;
  readonly inner: Schema<Value>;
}

export function optional<Value>(inner: Schema<Value>): OptionalSchema<Value> {
  return Object.freeze({
    jsonSchema: inner.jsonSchema,
    optional: true as const,
    inner,
  });
}

type ObjectShape = Readonly<Record<string, Schema<unknown>>>;
type OptionalKeys<Shape extends ObjectShape> = {
  [Key in keyof Shape]: Shape[Key] extends OptionalSchema<unknown> ? Key : never;
}[keyof Shape];
type RequiredKeys<Shape extends ObjectShape> = Exclude<keyof Shape, OptionalKeys<Shape>>;
type ObjectValue<Shape extends ObjectShape> = {
  readonly [Key in RequiredKeys<Shape>]: Infer<Shape[Key]>;
} & {
  readonly [Key in OptionalKeys<Shape>]?: Exclude<Infer<Shape[Key]>, undefined>;
};

export function object<const Shape extends ObjectShape>(
  shape: Shape,
): Schema<ObjectValue<Shape>> {
  const properties: Record<string, JsonSchema> = {};
  const required: string[] = [];
  for (const [name, declaration] of Object.entries(shape)) {
    properties[name] = declaration.jsonSchema;
    if (!("optional" in declaration && declaration.optional === true)) required.push(name);
  }
  return frozen({
    type: "object",
    properties,
    required,
    additionalProperties: false,
  });
}

function frozen<Value>(jsonSchema: JsonSchema): Schema<Value> {
  return Object.freeze({ jsonSchema: Object.freeze(jsonSchema) });
}
