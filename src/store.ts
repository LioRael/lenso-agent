import type { AgentStore, StoreTransaction, Tables } from "./types";

type State = { [K in keyof Tables]: Map<string, Tables[K]> };

function emptyState(): State {
  return {
    sessions: new Map(),
    messages: new Map(),
    runs: new Map(),
    calls: new Map(),
    actions: new Map(),
  };
}

function copy<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

/** Reject thenables as well as native promises, and consume late rejections. */
export function assertSynchronous(value: unknown): void {
  if (value !== null && (typeof value === "object" || typeof value === "function") &&
    typeof Reflect.get(value, "then") === "function") {
    void Promise.resolve(value).catch(() => {});
    throw new Error("Agent store transactions must be synchronous");
  }
}

export function createMemoryStore(): AgentStore {
  let state = emptyState();
  let closed = false;
  let active = false;

  return {
    transaction<T>(work: (tx: StoreTransaction) => T): T {
      if (closed) throw new Error("Agent store is closed");
      if (active) throw new Error("Nested agent store transactions are not supported");
      const working = emptyState();
      for (const table of Object.keys(state) as (keyof Tables)[]) {
        const target = working[table] as Map<string, Tables[keyof Tables]>;
        for (const [id, record] of state[table]) target.set(id, copy(record));
      }
      active = true;
      let live = true;
      const check = () => {
        if (!live) throw new Error("Agent store transaction has ended");
        if (closed) throw new Error("Agent store is closed");
      };
      const tx: StoreTransaction = {
        get<K extends keyof Tables>(table: K, id: string): Tables[K] | undefined {
          check();
          const record = working[table].get(id);
          return record === undefined ? undefined : copy(record);
        },
        list<K extends keyof Tables>(table: K, sessionId?: string): Tables[K][] {
          check();
          return Array.from(working[table].values())
            .filter(record => sessionId === undefined ||
              ("sessionId" in record ? record.sessionId : record.id) === sessionId)
            .map(record => copy(record));
        },
        put<K extends keyof Tables>(table: K, record: Tables[K]): void {
          check();
          const value = copy(record);
          working[table].set(value.id, value);
        },
      };
      try {
        const result = work(tx);
        assertSynchronous(result);
        state = working;
        return result;
      } finally {
        live = false;
        active = false;
      }
    },
    close(): void {
      if (active) throw new Error("Cannot close an active agent store transaction");
      closed = true;
    },
  };
}
