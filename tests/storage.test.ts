import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createMemoryStore } from "../src/store";
import { createSqliteStore, migrateAgentDatabase } from "../src/sqlite";
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
  });
}

describe("SQLite database integration", () => {
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
    for (const version of [0, 2]) {
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
        expect(tx.list("runs", "session")).toEqual([]);
        let failure: unknown;
        try {
          second.transaction(other => {
            competingCallbackEntered = true;
            if (other.list("runs", "session").length === 0)
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
        if (tx.list("runs", "session").some(run => run.status === "running")) return false;
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
