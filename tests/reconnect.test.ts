import { expect, test } from "bun:test";
import { RunEvents } from "../src/events";
import { watchRunEvents } from "../src/reconnect";
import type { AgentEvent, RunStatus } from "../src/types";

function checkpoint(sequence = 0, status: RunStatus = "running") {
  return {
    snapshot: {
      session: { id: "session", identity: { subject: "alice", scope: "personal", application: "test", target: "local" }, profile: "read", createdAt: 1 },
      runs: [{ id: "run", sessionId: "session", status, createdAt: 1 }],
      messages: [], calls: [], actions: [],
    },
    run: { id: "run", sessionId: "session", status, createdAt: 1 },
    sequence,
  };
}

function barrier() {
  let release!: () => void;
  const promise = new Promise<void>(resolve => { release = resolve; });
  return { promise, release };
}

function event(sequence: number, changes: Partial<AgentEvent> = {}): AgentEvent {
  return { id: `event-${sequence}`, sequence, timestamp: 1, sessionId: "session", runId: "run", kind: "text_delta", data: "[redacted]", ...changes };
}

function fixture(values: AgentEvent[], failure = false) {
  let reads = 0;
  let returns = 0;
  return {
    source: {
      subscribe(): AsyncIterableIterator<AgentEvent> {
        return {
          [Symbol.asyncIterator]() { return this; },
          async next() {
            reads++;
            if (failure) throw new Error("private-provider-value");
            const value = values.shift();
            return value ? { done: false, value } : { done: true, value: undefined };
          },
          async return() { returns++; values.length = 0; return { done: true, value: undefined }; },
        };
      },
    },
    reads: () => reads,
    returns: () => returns,
  };
}

test("snapshot then subscribe deterministically misses a terminal transition", async () => {
  const source = new RunEvents("session", "run", 4);
  let status = "running";
  const snapshot = { status };
  status = "completed";
  source.emit("run_completed");
  source.close();
  const next = await source.subscribe()[Symbol.asyncIterator]().next();
  expect(snapshot.status).toBe("running");
  expect(status).toBe("completed");
  expect(next.done).toBe(true);
});

test("subscribe-first retains a terminal emitted while an atomic checkpoint waits", async () => {
  const source = new RunEvents("session", "run", 4);
  const gate = barrier();
  const pending = watchRunEvents(source, async () => {
    const captured = checkpoint(source.sequence);
    await gate.promise;
    return captured;
  });
  source.emit("run_completed");
  source.close();
  gate.release();
  const watch = await pending;
  expect(watch.run.status).toBe("running");
  const iterator = watch.events[Symbol.asyncIterator]();
  expect((await iterator.next()).value?.kind).toBe("run_completed");
  await expect(iterator.next()).rejects.toThrow("resnapshot-required");
});

test("watermark discards reflected running events; terminal snapshot never regresses", async () => {
  const source = new RunEvents("session", "run", 4);
  const watch = await watchRunEvents(source, () => {
    source.emit("run_started");
    return checkpoint(source.sequence);
  });
  source.emit("run_completed");
  const iterator = watch.events[Symbol.asyncIterator]();
  expect(watch.sequence).toBe(1);
  expect((await iterator.next()).value?.kind).toBe("run_completed");
  await expect(iterator.next()).rejects.toThrow("resnapshot-required");

  const terminal = await watchRunEvents(source, () => {
    source.emit("run_started");
    return checkpoint(source.sequence, "completed");
  });
  expect(terminal.run.status).toBe("completed");
  expect((await terminal.events[Symbol.asyncIterator]().next()).done).toBe(true);
  await terminal.close();
});

test("missing or closed live source with running snapshot explicitly requires resnapshot", async () => {
  const closed = new RunEvents("session", "run", 2);
  closed.close();
  for (const source of [undefined, closed]) {
    const watch = await watchRunEvents(source, () => checkpoint());
    await expect(watch.events[Symbol.asyncIterator]().next()).rejects.toThrow("resnapshot-required");
    await watch.close();
  }
  const completed = await watchRunEvents(undefined, () => checkpoint(0, "completed"));
  expect((await completed.events[Symbol.asyncIterator]().next()).done).toBe(true);
});

test("overflow requires resnapshot even when its marker is at or below checkpoint", async () => {
  const source = new RunEvents("session", "run", 1);
  const watch = await watchRunEvents(source, () => {
    source.emit("text_delta");
    source.emit("text_delta");
    return checkpoint(source.sequence);
  });
  await expect(watch.events[Symbol.asyncIterator]().next()).rejects.toThrow("resnapshot-required");
  const fresh = source.subscribe()[Symbol.asyncIterator]();
  await fresh.return?.();
});

test("slow consumers stay bounded and do not pump a second unbounded queue", async () => {
  const source = new RunEvents("session", "run", 2);
  const watch = await watchRunEvents(source, () => checkpoint());
  for (let i = 0; i < 10000; i++) source.emit("text_delta");
  await expect(watch.events[Symbol.asyncIterator]().next()).rejects.toThrow("resnapshot-required");
  const fake = fixture([event(1), event(2)]);
  const idle = await watchRunEvents(fake.source, () => checkpoint());
  expect(fake.reads()).toBe(0);
  expect((await idle.events[Symbol.asyncIterator]().next()).value?.sequence).toBe(1);
  expect(fake.reads()).toBe(1);
  await idle.close();
  expect(fake.returns()).toBe(1);
});

test("repeated ID/sequence is deduplicated and live delivery remains contiguous", async () => {
  const fake = fixture([event(1), event(1), event(2, { kind: "run_completed" })]);
  const watch = await watchRunEvents(fake.source, () => checkpoint());
  const iterator = watch.events[Symbol.asyncIterator]();
  expect((await iterator.next()).value?.sequence).toBe(1);
  expect((await iterator.next()).value?.sequence).toBe(2);
  await expect(iterator.next()).rejects.toThrow("resnapshot-required");
  expect(fake.returns()).toBe(1);
});

test("live terminals require an authoritative refresh, including missing receipts and incomplete text prefixes", async () => {
  for (const kind of ["run_completed", "run_failed", "run_cancelled", "run_interrupted"] as const) {
    const source = new RunEvents("session", "run", 4);
    source.emit("text_delta", { data: { text: "First half. " } });
    const watch = await watchRunEvents(source, () => checkpoint(source.sequence));
    source.emit("text_delta", { data: { text: "Second half." } });
    source.emit(kind);
    source.close();
    const iterator = watch.events[Symbol.asyncIterator]();
    expect((await iterator.next()).value?.data).toEqual({ text: "Second half." });
    expect((await iterator.next()).value?.kind).toBe(kind);
    await expect(iterator.next()).rejects.toThrow("resnapshot-required");
    expect((await iterator.next()).done).toBe(true);
  }
});

test("gap, reused ID at new sequence, invalid sequence and wrong association require resnapshot", async () => {
  for (const values of [
    [event(2)],
    [event(1), event(2, { id: "event-1" })],
    [event(1, { sequence: NaN })],
    [event(1, { runId: "other" })],
    [event(1, { sessionId: "other" })],
  ]) {
    const fake = fixture(values);
    const watch = await watchRunEvents(fake.source, () => checkpoint());
    const iterator = watch.events[Symbol.asyncIterator]();
    if (values.length === 2) await iterator.next();
    await expect(iterator.next()).rejects.toThrow("resnapshot-required");
    expect(fake.returns()).toBe(1);
  }
});

test("concurrent next is explicitly rejected and pending reader/resource are released", async () => {
  const source = new RunEvents("session", "run", 1);
  const watch = await watchRunEvents(source, () => checkpoint());
  const iterator = watch.events[Symbol.asyncIterator]();
  const pending = iterator.next();
  await expect(iterator.next()).rejects.toThrow("concurrent-event-read");
  expect((await pending).done).toBe(true);
  const fresh = source.subscribe()[Symbol.asyncIterator]();
  const first = fresh.next();
  await expect(fresh.next()).rejects.toThrow("concurrent-event-read");
  expect((await first).done).toBe(true);
});

test("abort during snapshot promptly detaches without waiting for callback", async () => {
  const source = new RunEvents("session", "run", 1);
  const gate = barrier();
  const signal = new AbortController();
  const pending = watchRunEvents(source, async () => {
    await gate.promise;
    return checkpoint();
  }, { signal: signal.signal });
  signal.abort();
  await expect(pending).rejects.toThrow("watch-aborted");
  const fresh = source.subscribe()[Symbol.asyncIterator]();
  await fresh.return?.();
  gate.release();
});

test("preaborted signal does not subscribe/read; live abort, return and close release slots", async () => {
  const preaborted = new AbortController();
  preaborted.abort();
  let snapshots = 0;
  const fake = fixture([]);
  await expect(watchRunEvents(fake.source, () => { snapshots++; return checkpoint(); }, { signal: preaborted.signal })).rejects.toThrow("watch-aborted");
  expect(snapshots).toBe(0);
  expect(fake.returns()).toBe(0);
  for (const mode of ["abort", "return", "close"]) {
    const source = new RunEvents("session", "run", 1);
    const signal = new AbortController();
    const watch = await watchRunEvents(source, () => checkpoint(), { signal: signal.signal });
    const iterator = watch.events[Symbol.asyncIterator]();
    const pending = iterator.next();
    if (mode === "abort") signal.abort();
    else if (mode === "return") await iterator.return?.();
    else await watch.close();
    expect((await pending).done).toBe(true);
    await watch.close();
    const fresh = source.subscribe()[Symbol.asyncIterator]();
    await fresh.return?.();
  }
});

test("snapshot sync/async failures and source failures sanitize errors and detach", async () => {
  for (const readSnapshot of [
    () => { throw new Error("private-snapshot"); },
    async () => { throw new Error("private-snapshot"); },
  ]) {
    const fake = fixture([]);
    await expect(watchRunEvents(fake.source, readSnapshot)).rejects.toThrow("watch-snapshot-failed");
    expect(fake.returns()).toBe(1);
  }
  const fake = fixture([], true);
  const watch = await watchRunEvents(fake.source, () => checkpoint());
  await expect(watch.events[Symbol.asyncIterator]().next()).rejects.toThrow("resnapshot-required");
  expect(fake.returns()).toBe(1);
});

test("invalid checkpoint and iterator throw both release the registered consumer", async () => {
  for (const captured of [checkpoint(-1), { ...checkpoint(), run: { ...checkpoint().run, sessionId: "other" } }]) {
    const fake = fixture([]);
    await expect(watchRunEvents(fake.source, () => captured)).rejects.toThrow("resnapshot-required");
    expect(fake.returns()).toBe(1);
  }
  const source = new RunEvents("session", "run", 1);
  const watch = await watchRunEvents(source, () => checkpoint());
  await expect(watch.events[Symbol.asyncIterator]().throw?.(new Error("private-consumer-error"))).rejects.toThrow("event-stream-closed");
  const fresh = source.subscribe()[Symbol.asyncIterator]();
  await fresh.return?.();
});
