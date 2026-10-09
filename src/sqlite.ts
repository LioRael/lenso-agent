import { Database } from "bun:sqlite";
import { readFileSync } from "node:fs";
import { AgentError } from "./safety";
import { assertSynchronous, serializeRecord, validatePageOptions } from "./store";
import type { AgentStore, StoreFilter, StorePage, StorePageOptions, StoreTransaction, Tables } from "./types";

const tableNames = {
  sessions: "lenso_agent_sessions",
  messages: "lenso_agent_messages",
  runs: "lenso_agent_runs",
  calls: "lenso_agent_calls",
  actions: "lenso_agent_actions",
} as const;
const schemaVersion = 2;
const migration = (name: string) => readFileSync(new URL(`../migrations/${name}`, import.meta.url), "utf8");
type SchemaObject = { type: string; name: string; tbl_name: string; sql: string | null };
const schemaObjects = (db: Database): SchemaObject[] => db.query<SchemaObject, []>(
  "SELECT type, name, tbl_name, sql FROM sqlite_master WHERE name GLOB 'lenso_agent_*' OR tbl_name GLOB 'lenso_agent_*' ORDER BY type, name",
).all().map(row => ({ ...row, sql: row.sql?.replace(/\s+/g, " ").trim() ?? null }));
const expectedSchemas = new Map<number, SchemaObject[]>();
function expectedSchema(version: number): SchemaObject[] {
  let expected = expectedSchemas.get(version);
  if (!expected) {
    const reference = new Database(":memory:");
    try {
      reference.exec(migration("001-agent.sql"));
      if (version === 2) reference.exec(migration("002-storage-metadata.sql"));
      expected = schemaObjects(reference);
      expectedSchemas.set(version, expected);
    } finally {
      reference.close();
    }
  }
  return expected;
}

function requireSchema(db: Database, version = schemaVersion): void {
  const versions = db.query<{ singleton: number; version: number }, []>(
    "SELECT singleton, version FROM lenso_agent_schema",
  ).all();
  if (versions.length !== 1 || versions[0].singleton !== 1 || versions[0].version !== version)
    throw new Error("Unsupported agent database schema version");
  if (JSON.stringify(schemaObjects(db)) !== JSON.stringify(expectedSchema(version)))
    throw new Error("Invalid agent database schema");
}

/** Host-controlled migration; never changes the host's user_version or pragmas. */
export function migrateAgentDatabase(db: Database): void {
  if (db.inTransaction) throw new Error("Cannot migrate inside an existing database transaction");
  db.exec("BEGIN IMMEDIATE");
  try {
    const existing = db.query<{ name: string }, []>(
      "SELECT name FROM sqlite_master WHERE name GLOB 'lenso_agent_*'",
    ).all();
    if (existing.length === 0) {
      db.exec(migration("001-agent.sql"));
    } else if (!existing.some(row => row.name === "lenso_agent_schema")) {
      throw new Error("Agent database schema version is missing");
    }
    const version = db.query<{ version: number }, []>("SELECT version FROM lenso_agent_schema WHERE singleton = 1").get()?.version;
    if (version === 1) {
      requireSchema(db, 1);
      db.exec(migration("002-storage-metadata.sql"));
      for (const table of Object.keys(tableNames) as (keyof Tables)[]) {
        const name = tableNames[table];
        if (db.query(`SELECT 1 FROM ${name} WHERE sequence <= 0 OR sequence > ? LIMIT 1`).get(Number.MAX_SAFE_INTEGER))
          throw new Error(`Invalid agent database sequence: ${name}`);
        let after = 0;
        while (true) {
          const rows = db.query<{ sequence: number; id: string; session_id: string; value: string }, [number]>(
            `SELECT sequence, id, session_id, value FROM ${name} WHERE sequence > ? ORDER BY sequence LIMIT 1000`,
          ).all(after);
          if (rows.length === 0) break;
          for (const row of rows) {
            const value = serializeRecord(table, JSON.parse(row.value) as Tables[typeof table]);
            if (value.id !== row.id || value.sessionId !== row.session_id)
              throw new Error(`Invalid agent database record: ${name}`);
            db.query(`UPDATE ${name} SET value = ?, status = ?, bytes = ?, message_value_bytes = ? WHERE id = ?`)
              .run(value.json, value.status, value.bytes, value.messageValueBytes, value.id);
          }
          after = rows.at(-1)!.sequence;
        }
      }
    }
    requireSchema(db);
    db.exec("COMMIT");
  } catch (error) {
    db.exec("ROLLBACK");
    throw error;
  }
}

export function createSqliteStore(db: Database, options: { owned?: boolean } = {}): AgentStore {
  requireSchema(db);
  const owned = options.owned === true;
  let closed = false;
  let active = false;

  const sqlName = (table: keyof Tables): string => {
    if (!Object.hasOwn(tableNames, table)) throw new Error("Unknown agent store table");
    return tableNames[table];
  };
  return {
    transaction<T>(work: (tx: StoreTransaction) => T): T {
      if (closed) throw new Error("Agent store is closed");
      if (active || db.inTransaction) throw new Error("Nested agent store transactions are not supported");
      db.exec("BEGIN IMMEDIATE");
      active = true;
      let live = true;
      const check = () => {
        if (!live) throw new Error("Agent store transaction has ended");
        if (closed) throw new Error("Agent store is closed");
      };
      const tx: StoreTransaction = {
        get<K extends keyof Tables>(table: K, id: string): Tables[K] | undefined {
          check();
          const row = db.query<{ value: string }, [string]>(
            `SELECT value FROM ${sqlName(table)} WHERE id = ?`,
          ).get(id);
          return row === null ? undefined : JSON.parse(row.value) as Tables[K];
        },
        list<K extends keyof Tables>(table: K, sessionId?: string): Tables[K][] {
          check();
          const name = sqlName(table);
          const rows = sessionId === undefined
            ? db.query<{ value: string }, []>(`SELECT value FROM ${name} ORDER BY sequence`).all()
            : db.query<{ value: string }, [string]>(
              `SELECT value FROM ${name} WHERE session_id = ? ORDER BY sequence`,
            ).all(sessionId);
          return rows.map(row => JSON.parse(row.value) as Tables[K]);
        },
        count<K extends keyof Tables>(table: K, filter: StoreFilter = {}): number {
          check();
          const clauses: string[] = [], parameters: string[] = [];
          if (filter.sessionId !== undefined) { clauses.push("session_id = ?"); parameters.push(filter.sessionId); }
          if (filter.status !== undefined) { clauses.push("status = ?"); parameters.push(filter.status); }
          return db.query<{ count: number }, string[]>(
            `SELECT COUNT(*) AS count FROM ${sqlName(table)}${clauses.length ? ` WHERE ${clauses.join(" AND ")}` : ""}`,
          ).get(...parameters)!.count;
        },
        bytes<K extends keyof Tables>(table: K, sessionId: string, messageValues = false): number {
          check();
          const column = table === "messages" && messageValues ? "message_value_bytes" : "bytes";
          return db.query<{ bytes: number }, [string]>(
            `SELECT 2 + COALESCE(SUM(${column}), 0) + MAX(0, COUNT(*) - 1) AS bytes
             FROM ${sqlName(table)} WHERE session_id = ?`,
          ).get(sessionId)!.bytes;
        },
        page<K extends keyof Tables>(table: K, options: StorePageOptions): StorePage<Tables[K]> {
          check();
          validatePageOptions(options);
          const name = sqlName(table);
          const resolveId = (id: string): number => {
            if (id === "") return 0;
            const row = db.query<{ sequence: number }, [string, string]>(
              `SELECT sequence FROM ${name} WHERE session_id = ? AND id = ?`,
            ).get(options.sessionId, id);
            if (!row) throw new AgentError("invalid-input");
            return row.sequence;
          };
          const after = options.afterId === undefined ? options.after ?? 0 : resolveId(options.afterId);
          const bound = options.throughId === undefined
            ? options.through ?? Number.MAX_SAFE_INTEGER : resolveId(options.throughId);
          const last = db.query<{ sequence: number; id: string }, [string, number]>(
            `SELECT sequence, id FROM ${name} WHERE session_id = ? AND sequence <= ? ORDER BY sequence DESC LIMIT 1`,
          ).get(options.sessionId, bound);
          const through = options.through ?? (options.throughId === undefined ? last?.sequence ?? 0 : bound);
          if (after > through) throw new AgentError("invalid-input");
          // First fetch only bounded metadata. JSON never crosses the DB boundary before budgeting.
          const candidates = db.query<{ sequence: number; bytes: number }, [string, number, number, number]>(
            `SELECT sequence, bytes FROM ${name}
             WHERE session_id = ? AND sequence > ? AND sequence <= ? ORDER BY sequence LIMIT ?`,
          ).all(options.sessionId, after, through, options.limit + 1);
          let bytes = 2, hasMore = false;
          const selected: number[] = [];
          for (const row of candidates) {
            if (selected.length === options.limit) { hasMore = true; break; }
            if (row.bytes + 2 > options.maxBytes) throw new AgentError("snapshot-budget");
            const size = row.bytes + (selected.length ? 1 : 0);
            if (bytes + size > options.maxBytes) { hasMore = true; break; }
            bytes += size;
            selected.push(row.sequence);
          }
          const cursor = selected.at(-1) ?? after;
          const rows = selected.length === 0 ? [] : db.query<{ value: string }, [string, number, number]>(
            `SELECT value FROM ${name} WHERE session_id = ? AND sequence > ? AND sequence <= ? ORDER BY sequence`,
          ).all(options.sessionId, after, cursor);
          return { items: rows.map(row => JSON.parse(row.value) as Tables[K]), cursor, through, throughId: last?.id ?? "", hasMore };
        },
        put<K extends keyof Tables>(table: K, record: Tables[K]): void {
          check();
          const name = sqlName(table);
          const value = serializeRecord(table, record);
          db.query(
            `INSERT INTO ${name} (id, session_id, value, status, bytes, message_value_bytes) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET session_id = excluded.session_id, value = excluded.value,
             status = excluded.status, bytes = excluded.bytes, message_value_bytes = excluded.message_value_bytes`,
          ).run(value.id, value.sessionId, value.json, value.status, value.bytes, value.messageValueBytes);
        },
      };
      try {
        const result = work(tx);
        assertSynchronous(result);
        db.exec("COMMIT");
        return result;
      } catch (error) {
        db.exec("ROLLBACK");
        throw error;
      } finally {
        live = false;
        active = false;
      }
    },
    close(): void {
      if (closed) return;
      if (active) throw new Error("Cannot close an active agent store transaction");
      if (owned) db.close();
      closed = true;
    },
  };
}
