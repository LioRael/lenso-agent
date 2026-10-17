import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createMcpStdioClient } from "../src/mcp";

// Pass the absolute path of an already-built lenso-mcp-demo. Never builds or edits Rust.
const command = process.argv[2];
assert(command?.startsWith("/"), "Usage: bun scripts/mcp-rust-smoke.ts /absolute/path/lenso-mcp-demo");
const client = createMcpStdioClient({ command, timeoutMs: 2000 });
const signal = new AbortController().signal;
const name = (method: string) => "operation_" + createHash("sha256").update(JSON.stringify(["notes", method])).digest("base64url");
try {
  const catalog = await client.listTools(signal);
  assert.deepEqual(catalog.tools.map(tool => tool.name).sort(), [name("echo"), name("wait")].sort());
  const echo = await client.callTool(name("echo"), { text: "local stdio acceptance" }, signal, catalog.revision);
  assert.deepEqual(echo.structuredContent, { echo: "local stdio acceptance" });
  assert.equal(echo.content[0]?.type, "text");
  const abort = new AbortController();
  const waiting = client.callTool(name("wait"), {}, abort.signal, catalog.revision).catch(error => error.code);
  // A second response proves this process has received both piped requests.
  await client.callTool(name("echo"), { text: "concurrent" }, signal, catalog.revision);
  abort.abort();
  assert.equal(await waiting, "outcome-unknown");
  const after = await client.callTool(name("echo"), { text: "after cancellation" }, signal, catalog.revision);
  assert.deepEqual(after.structuredContent, { echo: "after cancellation" });
  console.log("Rust stdio: discovery, stable names, structured echo, concurrent cancellation and connection reuse passed.");
} finally { await client.close(); }
