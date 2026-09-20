import { tool, tools } from "@lenso/agent-tool-sdk";
import * as schema from "@lenso/agent-tool-sdk/schema";

export default tools([
  tool(
    {
      name: "lookup_order",
      description: "Look up an order.",
      input: schema.object({ id: schema.string() }),
      output: schema.string(),
    },
    ({ id }) => ({ ok: true, value: `Order ${id}` }),
  ),
]);
