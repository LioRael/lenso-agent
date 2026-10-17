// Reuse the discovery peer with a TERM-resistant worker holding its output pipes.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";

setTimeout(() => process.exit(0), 10_000);
const ready = `${process.argv[4]}.${process.pid}`;
spawn(process.execPath, [join(import.meta.dir, "mcp-process-tree.ts"), "worker", ready], {
  stdio: ["ignore", "inherit", "inherit"], env: { PATH: process.env.PATH },
});
while (!existsSync(ready)) await Bun.sleep(5);
await import("./mcp-server");
