// Local lifecycle fixture. Every process has a watchdog, including on assertion failure.
import { spawn } from "node:child_process";
import { existsSync, writeFileSync } from "node:fs";
import { createInterface } from "node:readline";

const [mode, ready] = process.argv.slice(2);
setTimeout(() => process.exit(0), 10_000);
if (mode === "worker") {
  process.on("SIGTERM", () => {});
  writeFileSync(ready!, String(process.pid));
} else {
  const worker = spawn(process.execPath, [import.meta.filename, "worker", ready!], {
    stdio: ["ignore", mode === "closed-pipes" ? "ignore" : "inherit", mode === "closed-pipes" ? "ignore" : "inherit"],
    env: { PATH: process.env.PATH },
  });
  while (!existsSync(ready!)) await Bun.sleep(5);
  if (mode === "stubborn") process.on("SIGTERM", () => {});
  let cancellations = 0;
  const reply = (id: number, result: unknown) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, result }) + "\n");
  createInterface({ input: process.stdin }).on("line", line => {
    const { id, method, params } = JSON.parse(line);
    if (method === "initialize") reply(id, { protocolVersion: params.protocolVersion, capabilities: { tools: {} } });
    if (method === "tools/list") reply(id, { tools: [{ name: "work", inputSchema: { type: "object" } }] });
    if (method === "tools/call") {
      if (params.arguments.crash) process.exit(0);
      if (!params.arguments.hang) reply(id, { content: [], structuredContent: { pid: process.pid, worker: worker.pid, cancellations } });
    }
    // Cancellation is deliberately ignored by the peer and its worker.
    if (method === "notifications/cancelled") cancellations++;
  });
}
