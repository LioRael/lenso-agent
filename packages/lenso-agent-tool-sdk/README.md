# `@lenso/agent-tool-sdk`

Agent-owned ToolProvider authoring for TypeScript Plugins. The generic Plugin SDK only sees the resulting provider declaration.

```ts
import { definePlugin } from "@lenso/bun-plugin";
import { tool, tools } from "@lenso/agent-tool-sdk";
import * as schema from "@lenso/agent-tool-sdk/schema";

const message = schema.object({ value: schema.string() });

export default definePlugin({
  providers: [
    tools([
      tool(
        {
          name: "uppercase",
          description: "Uppercase a message.",
          input: message,
          output: schema.string(),
        },
        (input) => ({ ok: true, value: input.value.toUpperCase() }),
      ),
    ]),
  ],
});
```

`tools` and `tool` are statically extracted. Plugin source is never evaluated while its provider binder is generated.
