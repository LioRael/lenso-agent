import { AgentError } from "./safety";
import type { AgentStore, StoreFilter, StorePage, StorePageOptions, StoreTransaction, Tables } from "./types";

const tableKeys = ["sessions", "messages", "runs", "calls", "actions"] as const;
export interface StoredValue {
  id: string;
  sessionId: string;
  status: string | null;
  json: string;
  bytes: number;
  messageValueBytes: number;
}
interface Entry extends StoredValue { sequence: number }
interface Totals {
  count: number;
  bytes: number;
  messageValueBytes: number;
  statuses: Map<string, number>;
}
interface TableState {
  rows: Map<string, Entry>;
  sessions: Map<string, Entry[]>;
  totals: Totals;
  sessionTotals: Map<string, Totals>;
  through: number;
}
interface Changes {
  rows: Map<string, Entry>;
  totals: Totals;
  sessionTotals: Map<string, Totals>;
  through: number;
}
const emptyTotals = (): Totals => ({ count: 0, bytes: 0, messageValueBytes: 0, statuses: new Map() });
function adjust(totals: Totals, row: Entry, direction: number): void {
  totals.count += direction;
  totals.bytes += direction * row.bytes;
  totals.messageValueBytes += direction * row.messageValueBytes;
  if (row.status !== null) {
    const count = (totals.statuses.get(row.status) ?? 0) + direction;
    if (count === 0) totals.statuses.delete(row.status);
    else totals.statuses.set(row.status, count);
  }
}
function sessionTotals(groups: Map<string, Totals>, id: string): Totals {
  let totals = groups.get(id);
  if (!totals) groups.set(id, totals = emptyTotals());
  return totals;
}
function track(state: Pick<TableState, "totals" | "sessionTotals">, row: Entry, direction: number): void {
  adjust(state.totals, row, direction);
  adjust(sessionTotals(state.sessionTotals, row.sessionId), row, direction);
}
function lowerBound(rows: Entry[], sequence: number): number {
  let left = 0, right = rows.length;
  while (left < right) {
    const middle = Math.floor((left + right) / 2);
    if (rows[middle].sequence < sequence) left = middle + 1;
    else right = middle;
  }
  return left;
}
export function serializeRecord<K extends keyof Tables>(table: K, record: Tables[K]): StoredValue {
  const json = JSON.stringify(record);
  const value = JSON.parse(json) as Tables[K];
  const sessionId = "sessionId" in value ? value.sessionId : value.id;
  if (typeof value.id !== "string" || typeof sessionId !== "string")
    throw new Error("Invalid agent store record identity");
  return {
    id: value.id, sessionId, json, bytes: Buffer.byteLength(json),
    status: "status" in value && typeof value.status === "string" ? value.status : null,
    messageValueBytes: table === "messages"
      ? Buffer.byteLength(JSON.stringify((value as Tables["messages"]).value) ?? "null") : 0,
  };
}
export function validatePageOptions(options: StorePageOptions): void {
  if (typeof options.sessionId !== "string" ||
    !Number.isSafeInteger(options.limit) || options.limit < 1 || options.limit > 1000 ||
    !Number.isSafeInteger(options.maxBytes) || options.maxBytes < 2 ||
    (options.after !== undefined && (!Number.isSafeInteger(options.after) || options.after < 0)) ||
    (options.through !== undefined && (!Number.isSafeInteger(options.through) || options.through < 0)) ||
    [options.afterId, options.throughId].some(id => id !== undefined && (typeof id !== "string" || id.length > 4096)) ||
    (options.afterId !== undefined && options.after !== undefined) ||
    (options.throughId !== undefined && options.through !== undefined) ||
    (options.through !== undefined && (options.after ?? 0) > options.through))
    throw new AgentError("invalid-input");
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
  const state = Object.fromEntries(tableKeys.map(table => [table, {
    rows: new Map(), sessions: new Map(), totals: emptyTotals(), sessionTotals: new Map(), through: 0,
  }])) as Record<keyof Tables, TableState>;
  let closed = false;
  let active = false;

  return {
    transaction<T>(work: (tx: StoreTransaction) => T): T {
      if (closed) throw new Error("Agent store is closed");
      if (active) throw new Error("Nested agent store transactions are not supported");
      const changes = new Map<keyof Tables, Changes>();
      const tableState = (table: keyof Tables): TableState => {
        if (!Object.hasOwn(state, table)) throw new Error("Unknown agent store table");
        return state[table];
      };
      const working = (table: keyof Tables): Changes => {
        let pending = changes.get(table);
        if (!pending) {
          pending = { rows: new Map(), totals: emptyTotals(), sessionTotals: new Map(), through: tableState(table).through };
          changes.set(table, pending);
        }
        return pending;
      };
      // Merge only this session's ordered history with the transaction's writes.
      function* entries(table: keyof Tables, sessionId?: string, after = 0, through = Infinity): Generator<Entry> {
        const committed = tableState(table);
        const pending = changes.get(table);
        const rows = sessionId === undefined
          ? [...committed.rows.values()] : committed.sessions.get(sessionId) ?? [];
        const writes = [...pending?.rows.values() ?? []]
          .filter(row => (sessionId === undefined || row.sessionId === sessionId) && row.sequence > after && row.sequence <= through)
          .sort((a, b) => a.sequence - b.sequence);
        let index = lowerBound(rows, after + 1), write = 0;
        while (index < rows.length || write < writes.length) {
          const old = rows[index], fresh = writes[write];
          if (fresh && (!old || fresh.sequence <= old.sequence)) {
            yield fresh;
            write++;
          } else {
            index++;
            if (old.sequence > through) break;
            if (!pending?.rows.has(old.id)) yield old;
          }
        }
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
          const committed = tableState(table);
          const row = changes.get(table)?.rows.get(id) ?? committed.rows.get(id);
          return row === undefined ? undefined : JSON.parse(row.json) as Tables[K];
        },
        list<K extends keyof Tables>(table: K, sessionId?: string): Tables[K][] {
          check();
          return [...entries(table, sessionId)].map(row => JSON.parse(row.json) as Tables[K]);
        },
        count<K extends keyof Tables>(table: K, filter: StoreFilter = {}): number {
          check();
          const committed = tableState(table), pending = changes.get(table);
          const base = filter.sessionId === undefined ? committed.totals : committed.sessionTotals.get(filter.sessionId);
          const delta = filter.sessionId === undefined ? pending?.totals : pending?.sessionTotals.get(filter.sessionId);
          return filter.status === undefined
            ? (base?.count ?? 0) + (delta?.count ?? 0)
            : (base?.statuses.get(filter.status) ?? 0) + (delta?.statuses.get(filter.status) ?? 0);
        },
        bytes<K extends keyof Tables>(table: K, sessionId: string, messageValues = false): number {
          check();
          const committed = tableState(table);
          const base = committed.sessionTotals.get(sessionId), delta = changes.get(table)?.sessionTotals.get(sessionId);
          const count = (base?.count ?? 0) + (delta?.count ?? 0);
          const key = table === "messages" && messageValues ? "messageValueBytes" : "bytes";
          return 2 + (base?.[key] ?? 0) + (delta?.[key] ?? 0) + Math.max(0, count - 1);
        },
        page<K extends keyof Tables>(table: K, options: StorePageOptions): StorePage<Tables[K]> {
          check();
          validatePageOptions(options);
          const committed = tableState(table), pending = changes.get(table);
          const resolveId = (id: string): number => {
            if (id === "") return 0;
            const row = pending?.rows.get(id) ?? committed.rows.get(id);
            if (!row || row.sessionId !== options.sessionId) throw new AgentError("invalid-input");
            return row.sequence;
          };
          const after = options.afterId === undefined ? options.after ?? 0 : resolveId(options.afterId);
          const bound = options.throughId === undefined
            ? options.through ?? pending?.through ?? committed.through : resolveId(options.throughId);
          if (after > bound) throw new AgentError("invalid-input");
          const rows = committed.sessions.get(options.sessionId) ?? [];
          let last: Entry | undefined;
          for (let index = lowerBound(rows, bound + 1) - 1; index >= 0; index--) {
            if (!pending?.rows.has(rows[index].id)) { last = rows[index]; break; }
          }
          for (const row of pending?.rows.values() ?? []) {
            if (row.sessionId === options.sessionId && row.sequence <= bound && (!last || row.sequence > last.sequence))
              last = row;
          }
          const through = options.through ?? (options.throughId === undefined ? last?.sequence ?? 0 : bound);
          if (after > through) throw new AgentError("invalid-input");
          const selected: Entry[] = [];
          let bytes = 2, hasMore = false;
          for (const row of entries(table, options.sessionId, after, through)) {
            if (selected.length === options.limit) { hasMore = true; break; }
            if (row.bytes + 2 > options.maxBytes) throw new AgentError("snapshot-budget");
            const size = row.bytes + (selected.length ? 1 : 0);
            if (bytes + size > options.maxBytes) { hasMore = true; break; }
            bytes += size;
            selected.push(row);
          }
          return {
            items: selected.map(row => JSON.parse(row.json) as Tables[K]),
            cursor: selected.at(-1)?.sequence ?? after, through, throughId: last?.id ?? "", hasMore,
          };
        },
        put<K extends keyof Tables>(table: K, record: Tables[K]): void {
          check();
          const value = serializeRecord(table, record);
          const pending = working(table);
          const old = pending.rows.get(value.id) ?? tableState(table).rows.get(value.id);
          const row = { ...value, sequence: old?.sequence ?? ++pending.through };
          if (old) track(pending, old, -1);
          track(pending, row, 1);
          pending.rows.set(row.id, row);
        },
      };
      try {
        const result = work(tx);
        assertSynchronous(result);
        for (const [table, pending] of changes) {
          const committed = state[table];
          for (const row of pending.rows.values()) {
            const old = committed.rows.get(row.id);
            if (old) {
              track(committed, old, -1);
              const rows = committed.sessions.get(old.sessionId)!;
              const index = lowerBound(rows, old.sequence);
              if (old.sessionId === row.sessionId) {
                rows[index] = row;
              } else {
                rows.splice(index, 1);
                if (rows.length === 0) {
                  committed.sessions.delete(old.sessionId);
                  committed.sessionTotals.delete(old.sessionId);
                }
              }
            }
            track(committed, row, 1);
            if (!old || old.sessionId !== row.sessionId) {
              let rows = committed.sessions.get(row.sessionId);
              if (!rows) committed.sessions.set(row.sessionId, rows = []);
              rows.splice(lowerBound(rows, row.sequence), 0, row);
            }
            committed.rows.set(row.id, row);
          }
          committed.through = pending.through;
        }
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
