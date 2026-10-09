import { Database } from "bun:sqlite";
import { readFileSync } from "node:fs";
import { cpus } from "node:os";
import { createMemoryStore } from "../src/store";
import { createSqliteStore, migrateAgentDatabase } from "../src/sqlite";
import type { AgentStore, StoreTransaction, Tables } from "../src/types";

// Store-boundary microbenchmark only: no runtime, network, model, or production database.
// --legacy freezes the original full-copy/full-parse algorithms for repeatable comparisons.
const legacy = process.argv.includes("--legacy");
const iterations = 30;
const keys = ["sessions", "messages", "runs", "calls", "actions"] as const;
const identity = { subject: "bench", scope: "bench", application: "bench", target: "bench" };
function fixture(id: string, sessionId = id): Tables {
  const prepared = {
    definition: {
      name: "lookup", description: "benchmark", inputSchema: {},
      target: { application: "bench", instance: "bench", plugin: "bench", operation: "lookup" },
      effect: "read" as const, confirmation: false,
    },
    input: { text: "测".repeat(170) }, precondition: null,
  };
  return {
    sessions: { id: sessionId, identity, profile: "bench", createdAt: 1 },
    messages: { id, sessionId, runId: "target-run", createdAt: 1, value: { role: "user", content: "测".repeat(170), timestamp: 1 } },
    runs: { id, sessionId, status: "completed", createdAt: 1 },
    calls: { id, sessionId, runId: "target-run", modelCallId: id, identity, prepared, status: "completed", createdAt: 1 },
    actions: { id, sessionId, runId: "target-run", callId: id, identity, prepared, status: "completed", digest: id, createdAt: 1, expiresAt: 2 },
  };
}
function legacyMemory(): AgentStore {
  const empty = () => Object.fromEntries(keys.map(key => [key, new Map()])) as { [K in keyof Tables]: Map<string, Tables[K]> };
  let state = empty();
  const copy = <T>(value: T): T => JSON.parse(JSON.stringify(value)) as T;
  return {
    transaction<T>(work: (tx: StoreTransaction) => T): T {
      const working = empty();
      for (const key of keys)
        for (const [id, row] of state[key])
          (working[key] as Map<string, Tables[keyof Tables]>).set(id, copy(row));
      const tx = {
        get: (key: keyof Tables, id: string) => {
          const row = working[key].get(id);
          return row === undefined ? undefined : copy(row);
        },
        list: (key: keyof Tables, sessionId?: string) => [...working[key].values()]
          .filter(row => sessionId === undefined || ("sessionId" in row ? row.sessionId : row.id) === sessionId).map(copy),
        put: (key: keyof Tables, row: Tables[keyof Tables]) => (working[key] as Map<string, Tables[keyof Tables]>).set(row.id, copy(row)),
      } as unknown as StoreTransaction;
      const result = work(tx);
      state = working;
      return result;
    },
    close() {},
  };
}
function legacySqlite(db: Database): AgentStore {
  db.exec(readFileSync(new URL("../migrations/001-agent.sql", import.meta.url), "utf8"));
  return {
    transaction<T>(work: (tx: StoreTransaction) => T): T {
      db.exec("BEGIN IMMEDIATE");
      try {
        const tx = {
          get: (key: keyof Tables, id: string) => {
            const row = db.query<{ value: string }, [string]>(`SELECT value FROM lenso_agent_${key} WHERE id = ?`).get(id);
            return row === null ? undefined : JSON.parse(row.value);
          },
          list: (key: keyof Tables, sessionId?: string) => {
            const rows = sessionId === undefined
              ? db.query<{ value: string }, []>(`SELECT value FROM lenso_agent_${key} ORDER BY sequence`).all()
              : db.query<{ value: string }, [string]>(`SELECT value FROM lenso_agent_${key} WHERE session_id = ? ORDER BY sequence`).all(sessionId);
            return rows.map(row => JSON.parse(row.value));
          },
          put: (key: keyof Tables, row: Tables[keyof Tables]) => {
            const json = JSON.stringify(row);
            const value = JSON.parse(json);
            db.query(`INSERT INTO lenso_agent_${key}(id,session_id,value) VALUES (?,?,?)
              ON CONFLICT(id) DO UPDATE SET session_id=excluded.session_id,value=excluded.value`)
              .run(value.id, "sessionId" in value ? value.sessionId : value.id, json);
          },
        } as unknown as StoreTransaction;
        const result = work(tx);
        db.exec("COMMIT");
        return result;
      } catch (error) {
        db.exec("ROLLBACK");
        throw error;
      }
    },
    close() { db.close(); },
  };
}

console.log(JSON.stringify({
  boundary: "store only, not runtime/online", mode: legacy ? "legacy" : "indexed",
  bun: Bun.version, platform: process.platform, arch: process.arch, cpu: cpus()[0]?.model, iterations,
  dataset: "0/1k/10k unrelated sessions, 5 records/session; target fixed at 82 records; admission inserts then completes one run",
  metrics: "elapsed milliseconds and JSON.parse calls; SQLite statement calls include BEGIN/COMMIT; not runtime latency or retained memory",
}));
for (const history of [0, 1_000, 10_000]) {
  for (const backend of ["memory", "sqlite"]) {
    const db = backend === "sqlite" ? new Database(":memory:") : undefined;
    if (db && !legacy) migrateAgentDatabase(db);
    const store = db
      ? legacy ? legacySqlite(db) : createSqliteStore(db, { owned: true })
      : legacy ? legacyMemory() : createMemoryStore();
    store.transaction(tx => {
      for (let i = 0; i < history; i++) {
        const rows = fixture(`history-${i}`);
        for (const key of keys) tx.put(key, rows[key]);
      }
      for (let i = 0; i < 20; i++) {
        const rows = fixture(`target-${i}`, "target");
        for (const key of keys) tx.put(key, rows[key]);
      }
      tx.put("runs", { ...fixture("target-run", "target").runs, status: "running" });
    });
    const tasks = {
      runRead: () => store.transaction(tx => tx.get("runs", "target-run")),
      admission: () => store.transaction(tx => {
        const total = legacy ? tx.list("runs").filter(run => run.status === "running").length : tx.count("runs", { status: "running" });
        const local = legacy ? tx.list("runs", "admission").filter(run => run.status === "running").length : tx.count("runs", { sessionId: "admission", status: "running" });
        if (total < 10 && local === 0) {
          tx.put("runs", { ...fixture("admitted", "admission").runs, status: "running" });
          tx.put("runs", fixture("admitted", "admission").runs);
        }
      }),
      snapshot: () => store.transaction(tx => {
        if (!legacy) {
          const bytes = keys.reduce((sum, key) => sum + tx.bytes(key, "target"), 0);
          if (bytes > 1_000_000) throw new Error("snapshot budget");
        }
        const result = keys.map(key => tx.list(key, "target"));
        if (legacy && Buffer.byteLength(JSON.stringify(result)) > 1_000_000) throw new Error("snapshot budget");
        return result;
      }),
    };
    for (const [task, work] of Object.entries(tasks)) {
      for (let i = 0; i < 3; i++) work();
      Bun.gc(true);
      let parses = 0, statements = 0;
      const parse = JSON.parse;
      const query = db?.query.bind(db), exec = db?.exec.bind(db);
      JSON.parse = ((...args: Parameters<typeof JSON.parse>) => { parses++; return parse(...args); }) as typeof JSON.parse;
      if (db && query && exec) {
        db.query = ((sql: string) => { statements++; return query(sql); }) as typeof db.query;
        db.exec = ((sql: string) => { statements++; return exec(sql); }) as typeof db.exec;
      }
      const start = performance.now();
      try { for (let i = 0; i < iterations; i++) work(); }
      finally {
        JSON.parse = parse;
        if (db && query && exec) { db.query = query; db.exec = exec; }
      }
      const msPerOperation = (performance.now() - start) / iterations;
      console.log(JSON.stringify({
        history, rows: history * 5, backend, task, msPerOperation: +msPerOperation.toFixed(4),
        parsesPerOperation: parses / iterations, sqliteStatementsPerOperation: db ? statements / iterations : null,
      }));
    }
    store.close();
  }
}
