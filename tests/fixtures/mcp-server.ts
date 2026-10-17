// A real local JSON-RPC stdio peer. No HTTP, model calls, OAuth or credentials.
import { createInterface } from "node:readline";
import { appendFileSync } from "node:fs";

const mode = process.argv[2], log = process.argv[3];
let initialized = false, changed = false, cancellations = 0, calls = 0, lists = 0;
const pending = new Set<number>();
const reply = (id: number, result: unknown) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, result }) + "\n");
const notify = () => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", method: "notifications/tools/list_changed" }) + "\n");
const tools = () => ["echo", "change", "hang", "stats", "crash", "error", "malformed", ...(changed ? ["added"] : ["removed"])].map(name => ({
  name, description: `${name}${changed ? " updated" : ""}`,
  inputSchema: { type: "object", properties: { value: { type: "string" } } },
  outputSchema: { type: "object" }, annotations: { readOnlyHint: true },
}));
createInterface({ input: process.stdin }).on("line", line => {
  const message = JSON.parse(line), { id, method, params } = message;
  if (method === "initialize") {
    if (mode === "init-hang") return;
    const initialize = () => reply(id, { protocolVersion: mode === "bad-version" ? "invalid" : params.protocolVersion,
      capabilities: { tools: { listChanged: true } }, serverInfo: { name: "local-fixture", version: "1" } });
    if (mode === "init-delay") setTimeout(initialize, 50); else initialize();
  } else if (method === "notifications/initialized") initialized = true;
  else if (method === "notifications/cancelled") {
    if (pending.delete(params.requestId)) { cancellations++; reply(params.requestId, { content: [] }); }
  } else if (method === "tools/list") {
    if (!initialized) process.exit(2);
    lists++;
    if (mode === "oversized") { process.stdout.write("x".repeat(8192)); return; }
    if (mode === "list-hang") { pending.add(id); return; }
    if (mode === "churning" || (mode === "page-change" && lists === 2)) { changed = true; notify(); }
    if (mode === "cycle") { reply(id, { tools: [], nextCursor: "repeat" }); return; }
    if (mode === "duplicate") { reply(id, { tools: [tools()[0], tools()[0]] }); return; }
    const offset = Number(params.cursor ?? 0), all = tools();
    reply(id, { tools: all.slice(offset, offset + 2), ...(offset + 2 < all.length ? { nextCursor: String(offset + 2) } : {}) });
  } else if (method === "tools/call") {
    calls++;
    if (log) appendFileSync(log, params.name + "\n");
    switch (params.name) {
      case "change": changed = true; notify(); reply(id, { content: [] }); break;
      case "hang": pending.add(id); break;
      case "crash": process.exit(0); break;
      case "error": reply(id, { content: [{ type: "text", text: "business failure" }], structuredContent: { ok: false }, isError: true }); break;
      case "malformed": reply(id, { structuredContent: [] }); break;
      case "stats": reply(id, { content: [], structuredContent: { calls, cancellations, lists, pid: process.pid, inherited: process.env.LENSO_MCP_TEST_SECRET ?? null } }); break;
      default: reply(id, { content: [{ type: "text", text: "fallback" }], structuredContent: { value: params.arguments.value ?? params.name, pid: process.pid } });
    }
  }
});
