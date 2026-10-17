import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createMcpStdioClient } from "../src/mcp-client";

const signal = new AbortController().signal;
function alive(pid: number): boolean {
  try { process.kill(pid, 0); return true; } catch { return false; }
}
async function within<T>(promise: Promise<T>, ms = 1500): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  try {
    return await Promise.race([promise, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error("cleanup-deadline")), ms);
    })]);
  } finally { clearTimeout(timer!); }
}

for (const mode of ["normal", "stubborn", "crash", "closed-pipes", "cancel", "timeout", "result-first"]) {
  test.skipIf(process.platform === "win32")(`stdio shutdown cleans owned descendants after ${mode}`, async () => {
    const dir = mkdtempSync(join(tmpdir(), "lenso-process-tree-"));
    const ready = join(dir, "worker.pid");
    const client = createMcpStdioClient({
      command: process.execPath, args: [join(import.meta.dir, "fixtures/mcp-process-tree.ts"), mode, ready], timeoutMs: 1000,
    });
    let pid: number | undefined;
    try {
      const catalog = await client.listTools(signal);
      const result = await client.callTool("work", {}, signal, catalog.revision);
      pid = result.structuredContent!.pid as number;
      const worker = Number(readFileSync(ready, "utf8"));
      expect(alive(worker)).toBe(true);
      const controller = new AbortController();
      const pending = client.callTool("work", { hang: mode !== "result-first", crash: mode === "crash" }, controller.signal, catalog.revision)
        .then(() => "completed", error => error.message);
      if (mode === "crash") expect(await within(pending)).toBe("outcome-unknown");
      if (mode === "cancel" || mode === "timeout" || mode === "result-first") {
        if (mode === "cancel") {
          // A second reply is a barrier proving the hanging request reached the peer.
          await client.callTool("work", {}, signal, catalog.revision);
          controller.abort();
        }
        expect(await within(pending)).toBe(mode === "result-first" ? "completed" : "outcome-unknown");
        controller.abort();
        const survivor = await client.callTool("work", {}, signal, catalog.revision);
        expect(survivor.structuredContent!.cancellations).toBe(mode === "result-first" ? 0 : 1);
        // Per-call cancellation must leave unrelated work and the shared peer usable.
        expect(alive(worker)).toBe(true);
      }
      await within(Promise.all([client.close(), client.close()]));
      expect(await pending).toBe(mode === "result-first" ? "completed" : "outcome-unknown");
      // Reparented children can briefly remain zombies after the group has been killed.
      for (let attempt = 0; alive(worker) && attempt < 100; attempt++) await Bun.sleep(5);
      expect(alive(pid)).toBe(false);
      expect(alive(worker)).toBe(false);
    } finally {
      // Kill only fixture PIDs; keep a failing regression from leaving test children behind.
      const pids = [pid];
      try { pids.push(Number(readFileSync(ready, "utf8"))); } catch {}
      for (const owned of pids) if (owned && alive(owned)) process.kill(owned, "SIGKILL");
      await within(client.close());
      rmSync(dir, { recursive: true, force: true });
    }
  });
}
