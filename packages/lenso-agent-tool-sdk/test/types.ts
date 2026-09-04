import { definePlugin } from "@lenso/bun-plugin";
import { tool, tools } from "../src/index.js";
import * as schema from "../src/schema.js";

const input = schema.object({
  message: schema.string(),
  suffix: schema.optional(schema.string()),
});

const stateful = tool(
  {
    name: "uppercase",
    input,
    output: schema.string(),
  },
  (value, _call, instance: { readonly prefix: string }) => ({
    ok: true,
    value: `${instance.prefix}${value.message.toUpperCase()}${value.suffix ?? ""}`,
  }),
);

definePlugin({
  create: () => ({ prefix: "> " }),
  providers: [tools([stateful])],
});

tool(
  { name: "wrong-output", input: schema.string(), output: schema.number() },
  // @ts-expect-error a successful value must match the output schema
  () => ({ ok: true, value: "not a number" }),
);
