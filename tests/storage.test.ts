import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createMemoryStore } from "../src/store";
import { createSqliteStore, migrateAgentDatabase } from "../src/sqlite";
import { AgentError } from "../src/safety";
import type { AgentStore, Session, StoreTransaction, Tables } from "../src/types";

const identity = { subject: "user", scope: "team", application: "app", target: "local" };
const session = (id = "session"): Session => ({ id, identity: { ...identity }, profile: "assistant", createdAt: 1 });
function records(): Tables {
  const prepared = {
    definition: {
      name: "lookup", description: "Lookup", inputSchema: {},
      target: { application: "app", instance: "local", plugin: "catalog", operation: "lookup" },
      effect: "read" as const, confirmation: false,
    },
    input: { ids: ["one"] }, precondition: null,
  };
  return {
    sessions: session(),
    messages: {
      id: "message", sessionId: "session", runId: "run", createdAt: 1,
      value: { role: "user", content: "Hello", timestamp: 1 },
    },
    runs: { id: "run", sessionId: "session", status: "running", createdAt: 1 },
    calls: {
      id: "call", sessionId: "session", runId: "run", modelCallId: "model-call",
      identity: { ...identity }, prepared, status: "prepared", createdAt: 1,
    },
    actions: {
      id: "action", callId: "call", sessionId: "session", runId: "run",
      identity: { ...identity }, prepared, digest: "digest", createdAt: 1, expiresAt: 100, status: "pending",
    },
  };
}
const tableKeys = ["sessions", "messages", "runs", "calls", "actions"] as const;
function putRecords(tx: StoreTransaction, values: Tables): void {
  for (const table of tableKeys) tx.put(table, values[table]);
}

const factories: Record<string, () => AgentStore> = {
  memory: createMemoryStore,
  sqlite: () => {
    const db = new Database(":memory:");
    migrateAgentDatabase(db);
    return createSqliteStore(db, { owned: true });
  },
};

for (const [name, factory] of Object.entries(factories)) {
  describe(name, () => {
    test("a thrown callback cannot leave partial inserts or overwrite committed rows", () => {
      const store = factory();
      try {
        store.transaction(tx => tx.put("sessions", session()));
        expect(() => store.transaction(tx => {
          putRecords(tx, records());
          tx.put("sessions", { ...session(), profile: "changed" });
          throw new Error("abort");
        })).toThrow("abort");
        store.transaction(tx => {
          expect(tx.get("sessions", "session")).toEqual(session());
          for (const table of tableKeys.filter(table => table !== "sessions"))
            expect(tx.list(table)).toEqual([]);
        });
        expect(store.transaction(() => 42)).toBe(42);
      } finally {
        store.close();
      }
    });

    test("async callbacks roll back before awaiting and cannot write after resuming", async () => {
      const store = factory();
      let continuationError: unknown;
      try {
        expect(() => store.transaction(async tx => {
          tx.put("sessions", session());
          await Promise.resolve();
          try {
            tx.put("sessions", session("late"));
          } catch (error) {
            continuationError = error;
            throw error;
          }
        })).toThrow("must be synchronous");
        await Promise.resolve();
        expect(continuationError).toBeInstanceOf(Error);
        expect((continuationError as Error).message).toContain("ended");
        expect(store.transaction(tx => tx.list("sessions"))).toEqual([]);
        expect(() => store.transaction(tx => {
          tx.put("sessions", session());
          return { then(resolve: (value: string) => void) { resolve("done"); } };
        })).toThrow("must be synchronous");
        expect(store.transaction(tx => tx.list("sessions"))).toEqual([]);
      } finally {
        store.close();
      }
    });

    test("mutating inputs, reads, lists, or callback results cannot bypass put", () => {
      const store = factory();
      const values = records();
      const original = records();
      try {
        const returned = store.transaction(tx => {
          putRecords(tx, values);
          values.sessions.identity.subject = "input-mutated";
          (values.calls.prepared.input as { ids: string[] }).ids.push("input-mutated");
          const read = tx.get("calls", "call")!;
          (read.prepared.input as { ids: string[] }).ids.push("get-mutated");
          const listed = tx.list("actions");
          listed[0].identity.subject = "list-mutated";
          expect(tx.get("calls", "call")).toEqual(original.calls);
          return tx.get("sessions", "session")!;
        });
        returned.identity.subject = "result-mutated";
        store.transaction(tx => {
          for (const table of tableKeys) expect(tx.get(table, original[table].id)).toEqual(original[table]);
        });
      } finally {
        store.close();
      }
    });

    test("message insertion order survives timestamp ties, updates, and session filtering", () => {
      const store = factory();
      try {
        store.transaction(tx => {
          tx.put("sessions", session());
          tx.put("sessions", session("other"));
          const message = records().messages;
          tx.put("messages", { ...message, id: "z" });
          tx.put("messages", { ...message, id: "unrelated", sessionId: "other" });
          tx.put("messages", { ...message, id: "a" });
          tx.put("messages", { ...message, id: "z", value: { role: "user", content: "updated", timestamp: 1 } });
          expect(tx.list("messages", "session").map(row => row.id)).toEqual(["z", "a"]);
          expect(tx.list("messages").map(row => row.id)).toEqual(["z", "unrelated", "a"]);
          expect(tx.list("messages", "missing")).toEqual([]);
          expect(tx.list("sessions", "session")).toEqual([session()]);
          expect(tx.get("messages", "missing")).toBeUndefined();
        });
      } finally {
        store.close();
      }
    });

    test("retained transaction handles cannot mutate data or read after commit and close", () => {
      const store = factory();
      const tx = store.transaction(tx => tx);
      expect(() => tx.put("sessions", session())).toThrow("ended");
      expect(() => tx.get("sessions", "session")).toThrow("ended");
      expect(() => tx.list("sessions")).toThrow("ended");
      expect(() => tx.count("sessions")).toThrow("ended");
      expect(() => tx.bytes("sessions", "session")).toThrow("ended");
      expect(() => tx.page("sessions", { sessionId: "session", limit: 1, maxBytes: 1000 })).toThrow("ended");
      store.close();
      store.close();
      expect(() => store.transaction(tx => tx.get("sessions", "session"))).toThrow("closed");
    });

    test("nested transactions or close cannot silently invalidate an active transaction", () => {
      const store = factory();
      try {
        store.transaction(tx => {
          tx.put("sessions", session());
          expect(() => store.transaction(() => {})).toThrow("Nested");
          expect(() => store.close()).toThrow("active");
        });
        expect(store.transaction(tx => tx.get("sessions", "session"))).toEqual(session());
      } finally {
        store.close();
      }
    });

    test("metadata includes pending writes, replacements, moves, and exact UTF8 array sizes", () => {
      const store = factory();
      const values = records();
      values.messages.value = { role: "user", content: "中🙂\n\"\\", timestamp: 1 };
      try {
        const verify = (tx: StoreTransaction) => {
          for (const table of tableKeys) {
            expect(tx.count(table)).toBe(tx.list(table).length);
            for (const sessionId of ["session", "other", "missing"]) {
              const rows = tx.list(table, sessionId);
              expect(tx.count(table, { sessionId })).toBe(rows.length);
              expect(tx.bytes(table, sessionId)).toBe(Buffer.byteLength(JSON.stringify(rows)));
              expect(tx.bytes(table, sessionId, true)).toBe(Buffer.byteLength(JSON.stringify(
                table === "messages" ? (rows as Tables["messages"][]).map(row => row.value) : rows,
              )));
              for (const status of ["running", "completed", "pending", "missing"])
                expect(tx.count(table, { sessionId, status })).toBe(rows.filter(row => "status" in row && row.status === status).length);
            }
          }
        };
        store.transaction(tx => {
          putRecords(tx, values);
          verify(tx);
          tx.put("messages", { ...values.messages, id: "second" });
          tx.put("runs", { ...values.runs, status: "completed" });
          tx.put("runs", { ...values.runs, sessionId: "other" });
          expect(tx.count("runs", { status: "running" })).toBe(1);
          expect(tx.count("runs", { sessionId: "session", status: "running" })).toBe(0);
          verify(tx);
        });
        store.transaction(verify);
        expect(() => store.transaction(tx => {
          tx.put("runs", { ...values.runs, id: "new" });
          tx.put("messages", { ...values.messages, value: { role: "user", content: "changed", timestamp: 1 } });
          verify(tx);
          throw new Error("rollback metadata");
        })).toThrow("rollback metadata");
        store.transaction(tx => {
          verify(tx);
          expect(tx.count("runs")).toBe(1);
          expect(tx.get("messages", "message")).toEqual(values.messages);
        });
      } finally { store.close(); }
    });

    test("pages bound records before copying, keep a stable insertion watermark, and see current updates", () => {
      const store = factory();
      const message = records().messages;
      const options = { sessionId: "session", limit: 1, maxBytes: 10_000 };
      try {
        store.transaction(tx => {
          tx.put("messages", { ...message, id: "z" });
          tx.put("messages", { ...message, id: "unrelated", sessionId: "other" });
          tx.put("messages", { ...message, id: "a" });
          tx.put("messages", { ...message, id: "b" });
        });
        const first = store.transaction(tx => tx.page("messages", options));
        expect(first.items.map(row => row.id)).toEqual(["z"]);
        expect(first.hasMore).toBe(true);
        first.items[0].value = { role: "user", content: "reference mutation", timestamp: 1 };
        store.transaction(tx => {
          tx.put("messages", { ...message, id: "late" });
          tx.put("messages", { ...message, id: "a", value: { role: "user", content: "current", timestamp: 1 } });
        });
        const second = store.transaction(tx => tx.page("messages", { ...options, after: first.cursor, through: first.through }));
        expect(second.items.map(row => row.id)).toEqual(["a"]);
        expect(second.items[0].value).toEqual({ role: "user", content: "current", timestamp: 1 });
        expect(second.through).toBe(first.through);
        const third = store.transaction(tx => tx.page("messages", { ...options, after: second.cursor, through: first.through }));
        expect(third.items.map(row => row.id)).toEqual(["b"]);
        expect(third.hasMore).toBe(false);
        const end = store.transaction(tx => tx.page("messages", { ...options, after: third.cursor, through: first.through }));
        expect(end.items).toEqual([]);
        expect(end.cursor).toBe(third.cursor);
        expect(end.hasMore).toBe(false);
        expect(store.transaction(tx => tx.get("messages", "z"))).toEqual({ ...message, id: "z" });
        expect(store.transaction(tx => tx.page("messages", { ...options, limit: 1000 })).items.map(row => row.id)).toEqual(["z", "a", "b", "late"]);
        expect(store.transaction(tx => tx.page("messages", { ...options, sessionId: "missing" })).items).toEqual([]);
      } finally { store.close(); }
    });

    test("record-ID cursors resolve only inside the requested session", () => {
      const store = factory();
      const message = records().messages;
      const options = { sessionId: "session", limit: 1, maxBytes: 10_000 };
      try {
        store.transaction(tx => {
          tx.put("messages", { ...message, id: "first" });
          tx.put("messages", { ...message, id: "last" });
          tx.put("messages", { ...message, id: "foreign", sessionId: "other" });
        });
        const first = store.transaction(tx => tx.page("messages", options));
        expect(first.throughId).toBe("last");
        const second = store.transaction(tx => tx.page("messages", { ...options, afterId: "first", throughId: first.throughId }));
        expect(second.items.map(item => item.id)).toEqual(["last"]);
        for (const id of ["foreign", "missing"]) {
          expect(() => store.transaction(tx => tx.page("messages", { ...options, afterId: id }))).toThrow("invalid-input");
          expect(() => store.transaction(tx => tx.page("messages", { ...options, throughId: id }))).toThrow("invalid-input");
        }
        expect(() => store.transaction(tx => tx.page("messages", { ...options, afterId: "last", throughId: "first" }))).toThrow("invalid-input");
        const empty = store.transaction(tx => tx.page("messages", { ...options, throughId: "" }));
        expect(empty.items).toEqual([]);
        expect(empty.throughId).toBe("");
      } finally { store.close(); }
    });

    test("page ranges merge pending inserts, updates, and session moves in sequence order", () => {
      const store = factory();
      const message = records().messages;
      const options = { sessionId: "session", limit: 1000, maxBytes: 100_000 };
      try {
        store.transaction(tx => {
          tx.put("messages", { ...message, id: "early", sessionId: "other" });
          tx.put("messages", { ...message, id: "middle" });
          tx.put("messages", { ...message, id: "moved-out" });
        });
        store.transaction(tx => {
          tx.put("messages", { ...message, id: "late" });
          tx.put("messages", { ...message, id: "middle", value: { role: "user", content: "updated", timestamp: 1 } });
          tx.put("messages", { ...message, id: "early" });
          tx.put("messages", { ...message, id: "moved-out", sessionId: "other" });
          const page = tx.page("messages", options);
          expect(page.items.map(row => row.id)).toEqual(["early", "middle", "late"]);
          expect(page.items).toEqual(tx.list("messages", "session"));
          expect(tx.page("messages", { ...options, after: page.cursor, through: page.through }).items).toEqual([]);
        });
        expect(store.transaction(tx => tx.page("messages", options)).items.map(row => row.id)).toEqual(["early", "middle", "late"]);
      } finally { store.close(); }
    });

    test("page UTF8 budget includes brackets and commas and rejects oversized rows before any parse", () => {
      const store = factory();
      const values = records();
      values.messages.value = { role: "user", content: "🙂中\n", timestamp: 1 };
      try {
        store.transaction(tx => {
          putRecords(tx, values);
          for (const table of tableKeys) tx.put(table, { ...values[table], id: values[table].id.toUpperCase() });
        });
        for (const table of tableKeys.filter(table => table !== "sessions")) {
          const rows = store.transaction(tx => tx.list(table, "session"));
          const one = Buffer.byteLength(JSON.stringify([rows[0]]));
          const all = Buffer.byteLength(JSON.stringify(rows));
          const partial = store.transaction(tx => tx.page(table, { sessionId: "session", limit: 1000, maxBytes: one }));
          expect(partial.items).toEqual([rows[0]]);
          expect(partial.hasMore).toBe(true);
          const full = store.transaction(tx => tx.page(table, { sessionId: "session", limit: 1000, maxBytes: all }));
          expect(full.items).toEqual(rows);
          expect(full.hasMore).toBe(false);
          let parses = 0;
          const original = JSON.parse;
          JSON.parse = ((...args: Parameters<typeof JSON.parse>) => { parses++; return original(...args); }) as typeof JSON.parse;
          try {
            expect(() => store.transaction(tx => tx.page(table, { sessionId: "session", limit: 1000, maxBytes: one - 1 }))).toThrow(AgentError);
            expect(() => store.transaction(tx => tx.page(table, { sessionId: "session", limit: 1000, maxBytes: one - 1 }))).toThrow("snapshot-budget");
            expect(parses).toBe(0);
          } finally { JSON.parse = original; }
        }
      } finally { store.close(); }
    });

    test("invalid page bounds fail closed, including unsafe cursors and unbounded limits", () => {
      const store = factory();
      try {
        const options = { sessionId: "session", limit: 1, maxBytes: 1000 };
        for (const invalid of [
          { limit: 0 }, { limit: -1 }, { limit: 1001 }, { limit: 1.5 }, { limit: Infinity },
          { maxBytes: 1 }, { maxBytes: NaN }, { after: -1 }, { through: -1 },
          { after: Number.MAX_SAFE_INTEGER + 1 }, { after: 2, through: 1 },
        ])
          expect(() => store.transaction(tx => tx.page("messages", { ...options, ...invalid }))).toThrow("invalid-input");
        const empty = store.transaction(tx => tx.page("messages", { ...options, maxBytes: 2 }));
        expect(empty).toEqual({ items: [], cursor: 0, through: 0, throughId: "", hasMore: false });
      } finally { store.close(); }
    });

    test("point reads, count, bytes, and bounded pages never parse unrelated history", () => {
      const store = factory();
      try {
        store.transaction(tx => {
          const values = records();
          for (let i = 0; i < 1000; i++) {
            tx.put("messages", { ...values.messages, id: `history-${i}`, sessionId: `other-${i}` });
            tx.put("runs", { ...values.runs, id: `history-${i}`, sessionId: `other-${i}`, status: "completed" });
          }
          putRecords(tx, values);
          tx.put("messages", { ...values.messages, id: "second" });
        });
        let parses = 0;
        const original = JSON.parse;
        JSON.parse = ((...args: Parameters<typeof JSON.parse>) => { parses++; return original(...args); }) as typeof JSON.parse;
        try {
          store.transaction(tx => {
            expect(tx.count("runs", { status: "running" })).toBe(1);
            expect(tx.count("runs", { sessionId: "session", status: "running" })).toBe(1);
            expect(tx.bytes("messages", "missing")).toBe(2);
            expect(tx.bytes("messages", "session")).toBeGreaterThan(2);
            expect(parses).toBe(0);
            expect(tx.get("runs", "run")!.status).toBe("running");
            expect(parses).toBe(1);
            expect(tx.page("messages", { sessionId: "session", limit: 1, maxBytes: 10_000 }).items).toHaveLength(1);
            expect(parses).toBe(2);
          });
        } finally { JSON.parse = original; }
      } finally { store.close(); }
    });
  });
}

describe("SQLite database integration", () => {
  test("explicit v1 migration preserves records and sequences while backfilling canonical byte metadata", () => {
    const db = new Database(":memory:");
    const values = records();
    values.messages.value = { role: "user", content: "中🙂", timestamp: 1 };
    try {
      db.exec(readFileSync(new URL("../migrations/001-agent.sql", import.meta.url), "utf8"));
      for (const table of tableKeys) {
        const row = values[table];
        db.query(`INSERT INTO lenso_agent_${table}(sequence,id,session_id,value) VALUES (?,?,?,?)`)
          .run(9, row.id, "session", JSON.stringify(row, null, 2));
      }
      db.query("INSERT INTO lenso_agent_messages(sequence,id,session_id,value) VALUES (?,?,?,?)")
        .run(27, "second", "session", JSON.stringify({ ...values.messages, id: "second" }));
      expect(() => createSqliteStore(db)).toThrow("schema version");
      migrateAgentDatabase(db);
      const store = createSqliteStore(db);
      store.transaction(tx => {
        for (const table of tableKeys) {
          const rows = tx.list(table, "session");
          expect(tx.get(table, values[table].id)).toEqual(values[table]);
          expect(tx.bytes(table, "session")).toBe(Buffer.byteLength(JSON.stringify(rows)));
        }
        expect(tx.bytes("messages", "session", true)).toBe(Buffer.byteLength(JSON.stringify(tx.list("messages", "session").map(row => row.value))));
        expect(tx.count("runs", { status: "running" })).toBe(1);
        const page = tx.page("messages", { sessionId: "session", limit: 1, maxBytes: 10_000 });
        expect(page.cursor).toBe(9);
        expect(page.through).toBe(27);
        tx.put("messages", { ...values.messages, id: "late" });
        expect(tx.page("messages", { sessionId: "session", after: page.cursor, through: page.through, limit: 1000, maxBytes: 10_000 }).items.map(row => row.id)).toEqual(["second"]);
      });
      expect(db.query("SELECT sequence FROM lenso_agent_messages ORDER BY sequence").all()).toEqual([
        { sequence: 9 }, { sequence: 27 }, { sequence: 28 },
      ]);
      store.close();
    } finally { db.close(); }
  });

  test("v1 upgrades validate constraints, indexes, triggers and record identities before adopting data", () => {
    const original = readFileSync(new URL("../migrations/001-agent.sql", import.meta.url), "utf8");
    for (const schema of [
      original.replace("CHECK (json_valid(value))", ""),
      original.replace("AUTOINCREMENT", ""),
      original + "\nCREATE TRIGGER unrelated_name AFTER INSERT ON lenso_agent_messages BEGIN SELECT 1; END;",
      original + "\nDROP INDEX lenso_agent_runs_session_id;",
      original,
    ]) {
      const db = new Database(":memory:");
      try {
        db.exec(schema);
        if (schema === original)
          db.query("INSERT INTO lenso_agent_messages(id,session_id,value) VALUES (?,?,?)")
            .run("wrong-id", "session", JSON.stringify(records().messages));
        const before = db.query("SELECT name,sql FROM sqlite_master ORDER BY name").all();
        expect(() => migrateAgentDatabase(db)).toThrow();
        expect(db.query("SELECT name,sql FROM sqlite_master ORDER BY name").all()).toEqual(before);
        expect(db.query("SELECT version FROM lenso_agent_schema").get()).toEqual({ version: 1 });
        expect(db.inTransaction).toBe(false);
      } finally { db.close(); }
    }
  });

  test("v1 backfill processes bounded batches and rolls all of them back on a late invalid record", () => {
    const db = new Database(":memory:");
    try {
      db.exec(readFileSync(new URL("../migrations/001-agent.sql", import.meta.url), "utf8"));
      const insert = db.query("INSERT INTO lenso_agent_messages(id,session_id,value) VALUES (?,?,?)");
      const message = records().messages;
      for (let i = 0; i < 1001; i++)
        insert.run(`message-${i}`, "session", JSON.stringify({ ...message, id: i === 1000 ? "wrong" : `message-${i}` }, null, 2));
      const first = db.query("SELECT value FROM lenso_agent_messages WHERE sequence = 1").get();
      expect(() => migrateAgentDatabase(db)).toThrow("Invalid agent database record");
      expect(db.query("SELECT version FROM lenso_agent_schema").get()).toEqual({ version: 1 });
      expect(db.query("SELECT value FROM lenso_agent_messages WHERE sequence = 1").get()).toEqual(first);
      expect(db.query<{ name: string }, []>("PRAGMA table_info(lenso_agent_messages)").all().map(row => row.name)).not.toContain("bytes");
      db.query("UPDATE lenso_agent_messages SET value = ? WHERE id = 'message-1000'")
        .run(JSON.stringify({ ...message, id: "message-1000" }));
      migrateAgentDatabase(db);
      const store = createSqliteStore(db);
      store.transaction(tx => {
        expect(tx.count("messages", { sessionId: "session" })).toBe(1001);
        expect(tx.bytes("messages", "session")).toBe(Buffer.byteLength(JSON.stringify(tx.list("messages", "session"))));
      });
      store.close();
    } finally { db.close(); }
  });

  test("SQLite uses status/session range indexes and never fetches JSON for a rejected page", () => {
    const db = new Database(":memory:");
    try {
      migrateAgentDatabase(db);
      const store = createSqliteStore(db);
      store.transaction(tx => tx.put("messages", {
        ...records().messages, value: { role: "user", content: "x".repeat(10_000), timestamp: 1 },
      }));
      for (const sql of [
        "SELECT COUNT(*) FROM lenso_agent_runs WHERE status = 'running'",
        "SELECT COUNT(*) FROM lenso_agent_runs WHERE session_id = 'session' AND status = 'running'",
        "SELECT sequence,bytes FROM lenso_agent_messages WHERE session_id = 'session' AND sequence > 0 AND sequence <= 100 ORDER BY sequence LIMIT 11",
      ]) {
        const plan = db.query<{ detail: string }, []>(`EXPLAIN QUERY PLAN ${sql}`).all().map(row => row.detail).join("\n");
        expect(plan).toContain(sql.includes("runs") ? "status_session" : "session_sequence");
        expect(plan).not.toContain("TEMP B-TREE");
      }
      const query = db.query.bind(db);
      const seen: string[] = [];
      db.query = ((sql: string) => { seen.push(sql); return query(sql); }) as typeof db.query;
      try {
        expect(() => store.transaction(tx => tx.page("messages", { sessionId: "session", limit: 10, maxBytes: 100 }))).toThrow("snapshot-budget");
        expect(seen.some(sql => /\bSELECT\s+value\b/i.test(sql))).toBe(false);
        expect(seen.some(sql => sql.includes("SELECT sequence, bytes"))).toBe(true);
      } finally { db.query = query; }
      store.close();
    } finally { db.close(); }
  });

  test("construction cannot create tables; explicit idempotent migration preserves host tables and configuration", () => {
    const db = new Database(":memory:");
    try {
      db.exec("CREATE TABLE host_items (id TEXT PRIMARY KEY); INSERT INTO host_items VALUES ('host'); PRAGMA user_version = 41; PRAGMA busy_timeout = 23");
      const before = db.query("SELECT name, sql FROM sqlite_master ORDER BY name").all();
      expect(() => createSqliteStore(db)).toThrow();
      expect(db.query("SELECT name, sql FROM sqlite_master ORDER BY name").all()).toEqual(before);
      migrateAgentDatabase(db);
      const migrated = db.query("SELECT name, sql FROM sqlite_master ORDER BY name").all();
      migrateAgentDatabase(db);
      expect(db.query("SELECT name, sql FROM sqlite_master ORDER BY name").all()).toEqual(migrated);
      expect(db.query("SELECT * FROM host_items").all()).toEqual([{ id: "host" }]);
      expect(db.query("PRAGMA user_version").get()).toEqual({ user_version: 41 });
      expect(db.query("PRAGMA busy_timeout").get()).toEqual({ timeout: 23 });
      for (const table of tableKeys)
        expect(db.query(`PRAGMA index_info(lenso_agent_${table}_session_id)`).all()).toEqual([
          { seqno: 0, cid: 2, name: "session_id" },
        ]);
    } finally {
      db.close();
    }
  });

  test("unsupported or missing schema versions fail closed instead of adopting an incompatible database", () => {
    for (const version of [0, 3]) {
      const db = new Database(":memory:");
      try {
        migrateAgentDatabase(db);
        db.query("UPDATE lenso_agent_schema SET version = ?").run(version);
        expect(() => createSqliteStore(db)).toThrow("schema version");
        expect(() => migrateAgentDatabase(db)).toThrow("schema version");
        expect(db.inTransaction).toBe(false);
        expect(db.query("SELECT version FROM lenso_agent_schema").get()).toEqual({ version });
      } finally {
        db.close();
      }
    }
    const db = new Database(":memory:");
    try {
      db.exec("CREATE TABLE lenso_agent_sessions (id TEXT)");
      expect(() => migrateAgentDatabase(db)).toThrow("version is missing");
      expect(() => createSqliteStore(db)).toThrow();
      expect(db.query("SELECT name FROM sqlite_master").all()).toEqual([{ name: "lenso_agent_sessions" }]);
    } finally {
      db.close();
    }
  });

  test("a version marker alone cannot hide missing tables, columns, indexes, or version rows", () => {
    const corruptions = [
      "DROP TABLE lenso_agent_messages",
      "ALTER TABLE lenso_agent_calls RENAME COLUMN value TO wrong",
      "DROP INDEX lenso_agent_actions_session_id",
      "DROP INDEX lenso_agent_runs_status_session",
      "DROP INDEX lenso_agent_messages_session_sequence",
      "ALTER TABLE lenso_agent_messages RENAME COLUMN message_value_bytes TO wrong_bytes",
      "CREATE TRIGGER host_agent_trigger AFTER INSERT ON lenso_agent_runs BEGIN SELECT 1; END",
      "DROP INDEX lenso_agent_actions_session_id; CREATE INDEX lenso_agent_actions_session_id ON lenso_agent_actions(session_id) WHERE id <> ''",
      `DROP TABLE lenso_agent_actions;
       CREATE TABLE lenso_agent_actions (sequence INTEGER PRIMARY KEY, id TEXT NOT NULL, session_id TEXT NOT NULL, value TEXT NOT NULL);
       CREATE UNIQUE INDEX partial_action_id ON lenso_agent_actions(id) WHERE id <> '';
       CREATE INDEX lenso_agent_actions_session_id ON lenso_agent_actions(session_id)`,
      "DELETE FROM lenso_agent_schema",
    ];
    for (const corruption of corruptions) {
      const db = new Database(":memory:");
      try {
        migrateAgentDatabase(db);
        db.exec(corruption);
        const before = db.query("SELECT name, sql FROM sqlite_master ORDER BY name").all();
        expect(() => createSqliteStore(db)).toThrow();
        expect(() => migrateAgentDatabase(db)).toThrow();
        expect(db.query("SELECT name, sql FROM sqlite_master ORDER BY name").all()).toEqual(before);
        expect(db.inTransaction).toBe(false);
      } finally {
        db.close();
      }
    }
  });

  test("closing a borrowed store cannot close the host database; owned stores close it explicitly", () => {
    const db = new Database(":memory:");
    migrateAgentDatabase(db);
    const options = { owned: false };
    const borrowed = createSqliteStore(db, options);
    options.owned = true;
    borrowed.close();
    expect(db.query("SELECT 1 AS alive").get()).toEqual({ alive: 1 });
    const defaultBorrowed = createSqliteStore(db);
    defaultBorrowed.close();
    expect(db.query("SELECT 1 AS alive").get()).toEqual({ alive: 1 });
    const owned = createSqliteStore(db, { owned: true });
    owned.close();
    owned.close();
    expect(() => db.query("SELECT 1").get()).toThrow();
  });

  test("store and migration cannot commit or roll back a host-owned transaction", () => {
    const db = new Database(":memory:");
    try {
      migrateAgentDatabase(db);
      const store = createSqliteStore(db);
      db.exec("BEGIN");
      expect(() => store.transaction(() => {})).toThrow("Nested");
      expect(() => migrateAgentDatabase(db)).toThrow("existing");
      expect(db.inTransaction).toBe(true);
      db.exec("ROLLBACK");
      store.close();
    } finally {
      db.close();
    }
  });

  test("reopening a real file preserves every table, nested JSON, and original message order", () => {
    const directory = mkdtempSync(join(tmpdir(), "lenso-agent-storage-"));
    const path = join(directory, "agent.sqlite");
    const values = records();
    let db = new Database(path);
    try {
      migrateAgentDatabase(db);
      const first = createSqliteStore(db, { owned: true });
      first.transaction(tx => {
        putRecords(tx, values);
        tx.put("messages", { ...values.messages, id: "second" });
        tx.put("messages", { ...values.messages, value: { role: "user", content: "updated", timestamp: 1 } });
      });
      first.close();
      db = new Database(path);
      const reopened = createSqliteStore(db);
      reopened.transaction(tx => {
        for (const table of tableKeys) {
          const expected: Tables[keyof Tables] = table === "messages"
            ? { ...values.messages, value: { role: "user", content: "updated", timestamp: 1 } }
            : values[table];
          expect(tx.get(table, values[table].id)).toEqual(expected);
          expect(tx.list(table, "session").length).toBe(table === "messages" ? 2 : 1);
        }
        expect(tx.list("messages").map(row => row.id)).toEqual(["message", "second"]);
      });
      reopened.close();
    } finally {
      db.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });

  test("overlapping separate handles cannot both admit a run using a stale empty-session check", () => {
    const directory = mkdtempSync(join(tmpdir(), "lenso-agent-admission-"));
    const path = join(directory, "agent.sqlite");
    const db1 = new Database(path);
    const db2 = new Database(path);
    try {
      // The host selects fail-fast contention; the store must not change it.
      db1.exec("PRAGMA busy_timeout = 0");
      db2.exec("PRAGMA busy_timeout = 0");
      migrateAgentDatabase(db1);
      const first = createSqliteStore(db1);
      const second = createSqliteStore(db2);
      let competingCallbackEntered = false;
      first.transaction(tx => {
        expect(tx.count("runs", { sessionId: "session", status: "running" })).toBe(0);
        let failure: unknown;
        try {
          second.transaction(other => {
            competingCallbackEntered = true;
            if (other.count("runs", { sessionId: "session", status: "running" }) === 0)
              other.put("runs", { ...records().runs, id: "competing" });
          });
        } catch (error) {
          failure = error;
        }
        expect(failure).toBeInstanceOf(Error);
        expect((failure as { code: string }).code).toBe("SQLITE_BUSY");
        expect(competingCallbackEntered).toBe(false);
        tx.put("runs", records().runs);
      });
      const admitted = second.transaction(tx => {
        if (tx.count("runs", { sessionId: "session", status: "running" }) !== 0) return false;
        tx.put("runs", { ...records().runs, id: "competing" });
        return true;
      });
      expect(admitted).toBe(false);
      expect(first.transaction(tx => tx.list("runs").map(run => run.id))).toEqual(["run"]);
      first.close();
      second.close();
    } finally {
      db1.close();
      db2.close();
      rmSync(directory, { recursive: true, force: true });
    }
  });
});
