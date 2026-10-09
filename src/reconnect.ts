import type { RunEvents } from "./events";
import { AgentError } from "./safety";
import type { AgentEvent, Run, RunWatch, SessionSnapshot } from "./types";

type Checkpoint = { snapshot: SessionSnapshot; run: Run; sequence: number };
const terminalEvents = new Set(["run_completed", "run_failed", "run_cancelled", "run_interrupted"]);
const recentIdLimit = 256;

/** The callback must capture snapshot, run and sequence together, without yielding. */
export async function watchRunEvents(
  source: Pick<RunEvents, "subscribe"> | undefined,
  readSnapshot: () => Checkpoint | Promise<Checkpoint>,
  options: { signal?: AbortSignal } = {},
): Promise<RunWatch> {
  const signal = options.signal;
  if (signal?.aborted) throw new AgentError("watch-aborted");
  const iterator = source?.subscribe()[Symbol.asyncIterator]();
  const recentIds = new Set<string>();
  let stopped = false;
  let needsSnapshot = false;
  let reading = false;
  let closing: Promise<void> | undefined;
  let abortSnapshot: (() => void) | undefined;
  const close = (): Promise<void> => {
    if (closing) return closing;
    stopped = true;
    recentIds.clear();
    signal?.removeEventListener("abort", aborted);
    closing = Promise.resolve().then(async () => {
      try { await iterator?.return?.(); }
      catch { throw new AgentError("event-stream-closed"); }
    });
    return closing;
  };
  const aborted = () => {
    void close().catch(() => {});
    abortSnapshot?.();
  };
  signal?.addEventListener("abort", aborted, { once: true });
  let checkpoint: Checkpoint;
  try {
    const cancelled = new Promise<never>((_, reject) => {
      abortSnapshot = () => reject(new AgentError("watch-aborted"));
    });
    // Attach rejection handling before invoking a possibly throwing callback.
    cancelled.catch(() => {});
    checkpoint = await Promise.race([readSnapshot(), cancelled]);
    if (signal?.aborted) throw new AgentError("watch-aborted");
    if (!Number.isSafeInteger(checkpoint.sequence) || checkpoint.sequence < 0 ||
        checkpoint.run.sessionId !== checkpoint.snapshot.session.id)
      throw new AgentError("resnapshot-required");
  } catch (error) {
    await close().catch(() => {});
    throw error instanceof AgentError ? error : new AgentError("watch-snapshot-failed");
  } finally {
    abortSnapshot = undefined;
  }
  const { snapshot, run, sequence } = checkpoint;
  let lastSequence = sequence;
  if (run.status !== "running") await close();
  const events: AsyncIterableIterator<AgentEvent> = {
    [Symbol.asyncIterator]() { return this; },
    async next() {
      if (reading) {
        await close();
        throw new AgentError("concurrent-event-read");
      }
      if (needsSnapshot) {
        needsSnapshot = false;
        throw new AgentError("resnapshot-required");
      }
      if (stopped) return { done: true, value: undefined };
      reading = true;
      try {
        while (!stopped) {
          const next = await iterator?.next();
          if (stopped) return { done: true, value: undefined };
          if (!next || next.done) throw new AgentError("resnapshot-required");
          const event = next.value;
          if (event.sessionId !== run.sessionId || event.runId !== run.id ||
              !Number.isSafeInteger(event.sequence) || event.sequence <= 0 ||
              (event.kind === "safe_error" && (event.data as { code?: unknown } | undefined)?.code === "event-overflow"))
            throw new AgentError("resnapshot-required");
          if (event.sequence <= lastSequence) continue;
          if (event.sequence !== lastSequence + 1 || recentIds.has(event.id))
            throw new AgentError("resnapshot-required");
          lastSequence = event.sequence;
          recentIds.add(event.id);
          if (recentIds.size > recentIdLimit) recentIds.delete(recentIds.values().next().value!);
          if (terminalEvents.has(event.kind)) {
            needsSnapshot = true;
            await close();
          }
          return { done: false, value: event };
        }
        return { done: true, value: undefined };
      } catch {
        await close().catch(() => {});
        throw new AgentError("resnapshot-required");
      } finally {
        reading = false;
      }
    },
    async return() { await close(); return { done: true, value: undefined }; },
    async throw() { await close(); throw new AgentError("event-stream-closed"); },
  };
  return { snapshot, run, sequence, events, close };
}
