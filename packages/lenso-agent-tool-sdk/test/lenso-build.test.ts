import { expect, test } from "bun:test";

import { lower } from "../src/lenso-build.ts";

const span = { file: "plugin.ts", start: 0, end: 100 } as const;
const sdk = {
  name: "@lenso/agent-tool-sdk",
  version: "0.1.0",
  integrity: "sha512-locked",
} as const;

const value = (payload: unknown) => ({ kind: "value" as const, value: payload, span });
const declaration = (export_name: string, args: ReadonlyArray<any>) => ({
  kind: "declaration" as const,
  package: sdk,
  export_name,
  arguments: args,
  span,
});

test("lowers typed tools into one exact ToolProvider binder", () => {
  const inputSchema = declaration("object", [
    value({ message: declaration("string", []) }),
  ]);
  const tool = declaration("tool", [
    value({
      name: value("uppercase"),
      description: value("Uppercase a message."),
      input: inputSchema,
      output: declaration("string", []),
      execution: value("parallel_safe"),
    }),
    { kind: "handler", reference: "handler:uppercase", span },
  ]);

  const output = lower({
    api_version: 1,
    package: sdk,
    export_name: "tools",
    arguments: [value([tool])],
    span,
  });

  expect(output.providers).toEqual([
    {
      capability_id: "lenso.agent.tool-provider@2",
      descriptor_version: "2.1.0",
      descriptor_digest:
        "sha256:8bfc7951a77a853b22d6a1a03d31d36a11844ba5d3526fec0934bf95977ad80d",
      binder: {
        module: "generated/agent-tool-provider.ts",
        export_name: "bindAgentTools",
      },
      handler_references: ["handler:uppercase"],
    },
  ]);
  expect(output.files[0]?.contents).toContain('input_schema_json: JSON.stringify(input)');
  expect(output.files[0]?.contents).toContain('content_type: "text"');
  expect(output.files[0]?.contents).not.toContain("output_schema_json");
  expect(() =>
    new Bun.Transpiler({ loader: "ts" }).transformSync(output.files[0]!.contents),
  ).not.toThrow();
});

test("rejects duplicate names before emitting a provider", () => {
  const shared = value({
    name: value("duplicate"),
    input: declaration("unknown", []),
    output: declaration("unknown", []),
  });
  const makeTool = (reference: string) =>
    declaration("tool", [shared, { kind: "handler", reference, span }]);

  expect(() =>
    lower({
      api_version: 1,
      package: sdk,
      export_name: "tools",
      arguments: [value([makeTool("one"), makeTool("two")])],
      span,
    }),
  ).toThrow("duplicate Tool name duplicate");
});

test("rejects nested declarations from a different locked SDK", () => {
  const wrongTool = {
    ...declaration("tool", []),
    package: { ...sdk, version: "0.2.0" },
  };
  expect(() =>
    lower({
      api_version: 1,
      package: sdk,
      export_name: "tools",
      arguments: [value([wrongTool])],
      span,
    }),
  ).toThrow("same locked Agent SDK");
});
