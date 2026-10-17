import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { AgentError, jsonCopy } from "./safety";

export interface McpTool {
  name: string;
  description?: string;
  inputSchema: Record<string, unknown>;
  outputSchema?: Record<string, unknown>;
}
export interface McpToolResult {
  content: Record<string, unknown>[];
  structuredContent?: Record<string, unknown>;
  isError?: boolean;
}
export interface McpStdioOptions {
  /** Trusted executable and arguments; no shell expansion. */
  command: string;
  args?: readonly string[];
  cwd?: string;
  /** Only PATH is inherited by default. Supply needed variables explicitly. */
  env?: Readonly<Record<string, string>>;
  timeoutMs?: number;
  maxMessageBytes?: number;
  maxTools?: number;
  maxPages?: number;
}
export interface McpCatalog { tools: McpTool[]; revision: number }
export interface McpStdioClient {
  listTools(signal: AbortSignal): Promise<McpCatalog>;
  callTool(name: string, input: Record<string, unknown>, signal: AbortSignal, revision: number): Promise<McpToolResult>;
  close(): Promise<void>;
}

function object(value: unknown): value is Record<string, any> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function positive(value: number | undefined, fallback: number): number {
  const result = value ?? fallback;
  if (!Number.isSafeInteger(result) || result < 1) throw new AgentError("invalid-mcp-options");
  return result;
}
function check(signal: AbortSignal): void {
  if (signal.aborted) throw new AgentError("cancelled");
}

/** A bounded stdio tools client. Disconnected calls are never replayed. */
export function createMcpStdioClient(options: McpStdioOptions): McpStdioClient {
  const timeout = positive(options.timeoutMs, 30_000);
  const maxBytes = positive(options.maxMessageBytes, 1024 * 1024);
  const maxTools = positive(options.maxTools, 1024);
  const maxPages = positive(options.maxPages, 100);
  if (!options.command || typeof options.command !== "string") throw new AgentError("invalid-mcp-options");
  const command = options.command, args = [...(options.args ?? [])], cwd = options.cwd;
  const env = { PATH: process.env.PATH, ...options.env };
  let closed = false, revision = 0, nextId = 0;
  let active: Connection | undefined;
  let connecting: Promise<Connection> | undefined;
  const processGroup = process.platform !== "win32";
  const children = new Set<Connection>();
  type Pending = { finish(error: AgentError | null, result?: unknown): void; invocation: boolean };
  type Connection = {
    child: ChildProcessWithoutNullStreams;
    pending: Map<number, Pending>;
    dead: boolean;
    stopped: Promise<void>;
    stop(): void;
  };

  function send(connection: Connection, message: unknown): void {
    const frame = JSON.stringify(message) + "\n";
    if (Buffer.byteLength(frame) > maxBytes) throw new AgentError("output-budget");
    if (connection.dead || !connection.child.stdin.writable) throw new AgentError("mcp-disconnected");
    connection.child.stdin.write(frame);
  }
  function disconnect(connection: Connection): void {
    if (connection.dead) return;
    connection.dead = true;
    if (active === connection) { active = undefined; revision++; }
    for (const pending of connection.pending.values())
      pending.finish(new AgentError(pending.invocation ? "outcome-unknown" : "mcp-disconnected"));
    connection.stop();
  }
  function request(connection: Connection, method: string, params: unknown, signal: AbortSignal): Promise<unknown> {
    check(signal);
    if (connection.dead) return Promise.reject(new AgentError("mcp-disconnected"));
    if (connection.pending.size >= 128) return Promise.reject(new AgentError("mcp-busy"));
    const id = ++nextId, invocation = method === "tools/call";
    return new Promise((resolve, reject) => {
      let sent = false;
      const finish = (error: AgentError | null, result?: unknown) => {
        if (!connection.pending.delete(id)) return;
        clearTimeout(timer);
        signal.removeEventListener("abort", abort);
        if (error) reject(error); else resolve(result);
      };
      const cancel = (code: string) => {
        if (sent) {
          try { send(connection, { jsonrpc: "2.0", method: "notifications/cancelled", params: { requestId: id, reason: code } }); }
          catch { disconnect(connection); }
        }
        finish(new AgentError(invocation && sent ? "outcome-unknown" : code));
      };
      const abort = () => cancel("cancelled");
      const timer = setTimeout(() => cancel("mcp-timeout"), timeout);
      connection.pending.set(id, { finish, invocation });
      signal.addEventListener("abort", abort, { once: true });
      try {
        check(signal);
        send(connection, { jsonrpc: "2.0", id, method, params });
        sent = true;
      } catch (error) { finish(error instanceof AgentError ? error : new AgentError("mcp-disconnected")); }
    });
  }
  function start(): Connection {
    // A private POSIX process group lets shutdown reach workers even if their
    // launcher exits first. Never signal the host's inherited process group.
    const child = spawn(command, args, { cwd, env, shell: false, stdio: "pipe", detached: processGroup });
    const closed = new Promise<void>(resolve => child.once("close", () => resolve()));
    let finishStop!: () => void;
    const connection: Connection = {
      child, pending: new Map(), dead: false,
      stopped: new Promise<void>(resolve => { finishStop = resolve; }),
      stop() {
        const kill = (signal: NodeJS.Signals) => {
          if (processGroup && child.pid !== undefined) {
            try { process.kill(-child.pid, signal); }
            catch (error) {
              if ((error as NodeJS.ErrnoException).code !== "ESRCH") child.kill(signal);
            }
          } else child.kill(signal);
        };
        kill("SIGTERM");
        // Keep this timer alive, including after the leader's close event: a
        // worker can ignore TERM and can also close its inherited stdio early.
        const timer = setTimeout(() => {
          kill("SIGKILL");
          child.stdin.destroy();
          child.stdout.destroy();
          child.stderr.destroy();
          void closed.then(finishStop);
        }, 250);
        if (!processGroup) void closed.then(() => { clearTimeout(timer); finishStop(); });
      },
    };
    children.add(connection);
    void connection.stopped.then(() => children.delete(connection));
    let buffer = Buffer.alloc(0);
    child.stdout.on("data", (chunk: Buffer) => {
      if (connection.dead) return;
      buffer = Buffer.concat([buffer, chunk]);
      try {
        let end: number;
        while ((end = buffer.indexOf(10)) !== -1) {
          if (end > maxBytes) throw new Error();
          const line = buffer.subarray(0, end).toString("utf8");
          buffer = buffer.subarray(end + 1);
          const message: unknown = JSON.parse(line);
          if (!object(message) || message.jsonrpc !== "2.0") throw new Error();
          if (typeof message.method === "string") {
            if (message.id !== undefined) {
              // No sampling, elicitation or roots capability is advertised.
              send(connection, message.method === "ping"
                ? { jsonrpc: "2.0", id: message.id, result: {} }
                : { jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "Method not supported" } });
            } else if (message.method === "notifications/tools/list_changed") revision++;
          } else if (typeof message.id === "number") {
            const pending = connection.pending.get(message.id);
            if (!pending) continue; // Late responses after timeout/cancellation.
            if (Object.hasOwn(message, "error") === Object.hasOwn(message, "result")) throw new Error();
            pending.finish(Object.hasOwn(message, "error")
              ? new AgentError(pending.invocation ? "outcome-unknown" : "operation-failed") : null, message.result);
          } else throw new Error();
        }
        if (buffer.length > maxBytes) throw new Error();
      } catch { disconnect(connection); }
    });
    // Drain diagnostics without exposing server messages or inheriting log instructions.
    child.stderr.resume();
    child.on("error", () => disconnect(connection));
    child.on("exit", () => disconnect(connection));
    child.stdin.on("error", () => disconnect(connection));
    child.stdout.on("error", () => disconnect(connection));
    child.stderr.on("error", () => disconnect(connection));
    child.stdout.on("end", () => disconnect(connection));
    return connection;
  }
  async function connect(signal: AbortSignal): Promise<Connection> {
    check(signal);
    if (closed) throw new AgentError("mcp-closed");
    if (active && !active.dead) return active;
    if (!connecting) {
      connecting = (async () => {
        const connection = start();
        try {
          const result = await request(connection, "initialize", {
            protocolVersion: "2025-11-25", capabilities: {},
            clientInfo: { name: "lenso-agent", version: "0.1.0" },
          }, new AbortController().signal);
          if (!object(result) || !["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"].includes(result.protocolVersion) ||
              !object(result.capabilities) || !object(result.capabilities.tools)) throw new AgentError("mcp-protocol");
          if (closed || connection.dead) throw new AgentError("mcp-disconnected");
          send(connection, { jsonrpc: "2.0", method: "notifications/initialized" });
          active = connection;
          revision++;
          return connection;
        } catch (error) { disconnect(connection); throw error; }
      })().finally(() => { connecting = undefined; });
    }
    // A cancelled waiter must not cancel another caller's initialization.
    const pending = connecting;
    return new Promise((resolve, reject) => {
      const abort = () => reject(new AgentError("cancelled"));
      signal.addEventListener("abort", abort, { once: true });
      pending.then(value => { if (signal.aborted) abort(); else resolve(value); }, reject)
        .finally(() => signal.removeEventListener("abort", abort));
      if (signal.aborted) abort();
    });
  }

  return {
    async listTools(signal) {
      const connection = await connect(signal);
      // List afresh, even when a server does not advertise listChanged.
      for (let attempt = 0; attempt < 3; attempt++) {
        const startRevision = revision, tools: McpTool[] = [], cursors = new Set<string>(), names = new Set<string>();
        let cursor: string | undefined, bytes = 0;
        for (let page = 0; page < maxPages; page++) {
          const result = await request(connection, "tools/list", cursor === undefined ? {} : { cursor }, signal);
          if (!object(result) || !Array.isArray(result.tools)) throw new AgentError("mcp-protocol");
          if (revision !== startRevision) break;
          bytes += Buffer.byteLength(JSON.stringify(result));
          if (bytes > maxBytes) throw new AgentError("output-budget");
          for (const tool of result.tools) {
            if (!object(tool) || typeof tool.name !== "string" || !tool.name || names.has(tool.name) ||
                !object(tool.inputSchema) || tool.inputSchema.type !== "object" ||
                (tool.description !== undefined && typeof tool.description !== "string") ||
                (tool.outputSchema !== undefined && !object(tool.outputSchema))) throw new AgentError("mcp-protocol");
            names.add(tool.name);
            tools.push(jsonCopy(tool) as McpTool);
            if (tools.length > maxTools) throw new AgentError("output-budget");
          }
          if (result.nextCursor === undefined) return { tools, revision: startRevision };
          if (typeof result.nextCursor !== "string" || cursors.has(result.nextCursor)) throw new AgentError("mcp-protocol");
          cursor = result.nextCursor;
          cursors.add(cursor!);
          if (page === maxPages - 1) throw new AgentError("output-budget");
        }
      }
      throw new AgentError("tool-conflict");
    },
    async callTool(name, input, signal, expectedRevision) {
      check(signal);
      // Invocation never reconnects: callers must discover and re-admit the new connection.
      if (!active || active.dead || revision !== expectedRevision) throw new AgentError("tool-conflict");
      const result = await request(active, "tools/call", { name, arguments: input }, signal);
      if (!object(result) || !Array.isArray(result.content) ||
          result.content.some(item => !object(item) || typeof item.type !== "string") ||
          (result.structuredContent !== undefined && !object(result.structuredContent)) ||
          (result.isError !== undefined && typeof result.isError !== "boolean")) throw new AgentError("outcome-unknown");
      return jsonCopy({ content: result.content,
        ...(result.structuredContent !== undefined ? { structuredContent: result.structuredContent } : {}),
        ...(result.isError !== undefined ? { isError: result.isError } : {}),
      }, maxBytes);
    },
    async close() {
      closed = true;
      for (const connection of children) disconnect(connection);
      await Promise.all([...children].map(connection => connection.stopped));
    },
  };
}
