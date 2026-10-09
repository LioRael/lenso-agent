import type { AgentEvent, AgentService, RunWatch } from "./types";
import { AgentError } from "./safety";

export interface AgentFetchOptions<C> {
  agent: AgentService<C>;
  /** Verify subject, scope, target and Origin/CSRF here. JSON is never identity evidence. */
  authenticate(request: Request): Promise<C>;
  prefix?: string;
  maxBodyBytes?: number;
}

class RequestError extends Error {
  constructor(readonly status: number, readonly code: string) { super(code); }
}

async function body(request: Request, maxBytes: number, fields: readonly string[]): Promise<Record<string, string>> {
  const length = request.headers.get("content-length");
  if (length && (!/^\d+$/.test(length) || Number(length) > maxBytes))
    throw new RequestError(413, "body-too-large");
  if (request.headers.get("content-type")?.split(";")[0].trim() !== "application/json")
    throw new RequestError(415, "json-required");
  const reader = request.body?.getReader();
  if (!reader) throw new RequestError(400, "invalid-body");
  const chunks: Uint8Array[] = [];
  let size = 0;
  try {
    while (true) {
      const chunk = await reader.read();
      if (chunk.done) break;
      size += chunk.value.byteLength;
      if (size > maxBytes) throw new RequestError(413, "body-too-large");
      chunks.push(chunk.value);
    }
  } catch (error) {
    await reader.cancel().catch(() => {});
    throw error;
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
  let value: unknown;
  try { value = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)); }
  catch { throw new RequestError(400, "invalid-body"); }
  if (!value || typeof value !== "object" || Array.isArray(value) ||
      Object.keys(value).length !== fields.length ||
      fields.some(field => typeof Reflect.get(value, field) !== "string"))
    throw new RequestError(400, "invalid-body");
  return value as Record<string, string>;
}

function json(value: unknown, status = 200): Response {
  return Response.json(value, { status, headers: { "cache-control": "no-store" } });
}

function serviceError(error: unknown): Response {
  if (!(error instanceof AgentError)) return json({ error: "request-rejected" }, 400);
  const statuses: Record<string, number> = {
    "not-found": 404,
    "forbidden": 403,
    "forbidden-operation": 403,
    "session-busy": 409,
    "confirmation-mismatch": 409,
    "precondition-conflict": 409,
    "action-executing": 409,
    "action-completed": 409,
    "action-cancelled": 409,
    "action-rejected": 409,
    "action-outcome_unknown": 409,
    "action-expired": 410,
    "sensitive-input": 400,
    "invalid-input": 400,
    "unknown-profile": 400,
    "input-budget": 413,
    "snapshot-budget": 413,
    "concurrency-budget": 429,
    "service-closed": 503,
    "too-many-subscribers": 429,
  };
  return Object.hasOwn(statuses, error.code)
    ? json({ error: error.code }, statuses[error.code])
    : json({ error: "request-rejected" }, 400);
}

function eventStream(events: AsyncIterable<AgentEvent>, signal: AbortSignal, watch?: RunWatch): Response {
  const iterator = events[Symbol.asyncIterator]();
  const encoder = new TextEncoder();
  let snapshotPending = Boolean(watch);
  const converged = watch?.run.status !== "running";
  let detached = false;
  let controller: ReadableStreamDefaultController<Uint8Array>;
  const detach = async () => {
    if (detached) return;
    detached = true;
    signal.removeEventListener("abort", aborted);
    try { await iterator.return?.(); }
    finally { await watch?.close(); }
  };
  const aborted = () => {
    if (!detached) controller.close();
    void detach().catch(() => {});
  };
  const stream = new ReadableStream<Uint8Array>({
    start(value) {
      controller = value;
      signal.addEventListener("abort", aborted, { once: true });
      if (signal.aborted) aborted();
    },
    async pull(value) {
      if (detached) return;
      try {
        if (snapshotPending && watch) {
          snapshotPending = false;
          value.enqueue(encoder.encode(`event: snapshot\ndata: ${JSON.stringify({
            snapshot: watch.snapshot, run: watch.run, sequence: watch.sequence,
          })}\n\n`));
          return;
        }
        const next = await iterator.next();
        if (detached) return;
        if (next.done) {
          if (watch && !converged) throw new AgentError("resnapshot-required");
          value.close();
          await detach().catch(() => {});
          return;
        }
        value.enqueue(encoder.encode(`id: ${next.value.sequence}\nevent: ${next.value.kind}\ndata: ${JSON.stringify(next.value)}\n\n`));
      } catch {
        if (!detached) {
          if (watch) {
            value.enqueue(encoder.encode('event: resnapshot_required\ndata: {"code":"resnapshot-required"}\n\n'));
            value.close();
          } else value.error(new Error("event-stream-failed"));
        }
        await detach().catch(() => {});
      }
    },
    cancel: detach,
  }, { highWaterMark: 0 });
  return new Response(stream, { headers: {
    "content-type": "text/event-stream; charset=utf-8",
    "cache-control": "no-store",
    "x-accel-buffering": "no",
  } });
}

/** Mount in the host's existing fetch function. No listener or authentication source is installed. */
export function createAgentFetchHandler<C>(options: AgentFetchOptions<C>): (request: Request) => Promise<Response | undefined> {
  const prefix = options.prefix ?? "/agent";
  const maxBytes = options.maxBodyBytes ?? 64 * 1024;
  if (!/^\/(?:[A-Za-z0-9_-]+\/)*[A-Za-z0-9_-]+$/.test(prefix))
    throw new Error("invalid-agent-prefix");
  if (!Number.isSafeInteger(maxBytes) || maxBytes <= 0) throw new Error("invalid-body-limit");
  return async request => {
    const path = new URL(request.url).pathname;
    if (!path.startsWith(`${prefix}/`)) return undefined;
    const parts = path.slice(prefix.length + 1).split("/");
    const id = parts[1];
    const route =
      parts.length === 1 && parts[0] === "sessions" ? "session" :
      id && /^[A-Za-z0-9_-]+$/.test(id) && parts.length === 2 && parts[0] === "sessions" ? "snapshot" :
      id && /^[A-Za-z0-9_-]+$/.test(id) && parts.length === 3 && parts[0] === "sessions" && parts[2] === "runs" ? "start" :
      id && /^[A-Za-z0-9_-]+$/.test(id) && parts.length === 2 && parts[0] === "runs" ? "run" :
      id && /^[A-Za-z0-9_-]+$/.test(id) && parts.length === 3 && parts[0] === "runs" && parts[2] === "events" ? "events" :
      id && /^[A-Za-z0-9_-]+$/.test(id) && parts.length === 3 && parts[0] === "runs" && parts[2] === "watch" ? "watch" :
      id && /^[A-Za-z0-9_-]+$/.test(id) && parts.length === 3 && parts[0] === "runs" && parts[2] === "cancel" ? "cancel-run" :
      id && /^[A-Za-z0-9_-]+$/.test(id) && parts.length === 3 && parts[0] === "actions" && parts[2] === "confirm" ? "confirm" :
      id && /^[A-Za-z0-9_-]+$/.test(id) && parts.length === 3 && parts[0] === "actions" && parts[2] === "cancel" ? "cancel-action" : undefined;
    if (!route) return undefined;
    let context: C;
    try { context = await options.authenticate(request); }
    catch { return json({ error: "unauthenticated" }, 401); }
    const method = ["snapshot", "run", "events", "watch"].includes(route) ? "GET" : "POST";
    if (request.method !== method) return new Response(null, { status: 405, headers: { allow: method } });
    try {
      switch (route) {
        case "session": return json(await options.agent.createSession(context, (await body(request, maxBytes, ["profile"])).profile), 201);
        case "snapshot": return json(await options.agent.readSession(context, id));
        case "start": {
          const handle = await options.agent.startRun(context, id, (await body(request, maxBytes, ["text"])).text);
          await handle.events[Symbol.asyncIterator]().return?.();
          return json(handle.run, 202);
        }
        case "run": return json(await options.agent.getRun(context, id));
        case "events": return eventStream(await options.agent.events(context, id), request.signal);
        case "watch": {
          const watch = await options.agent.watchRun(context, id, { signal: request.signal });
          return eventStream(watch.events, request.signal, watch);
        }
        case "confirm": return json(await options.agent.confirmAction(context, id, (await body(request, maxBytes, ["digest"])).digest));
        case "cancel-run": await body(request, maxBytes, []); return json(await options.agent.cancelRun(context, id));
        case "cancel-action": await body(request, maxBytes, []); return json(await options.agent.cancelAction(context, id));
      }
    } catch (error) {
      if (error instanceof RequestError) return json({ error: error.code }, error.status);
      // Never serialize domain/provider error messages or causes into the host transport.
      return serviceError(error);
    }
  };
}
