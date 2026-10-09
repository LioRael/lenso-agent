import { Database } from "bun:sqlite";
import { readFileSync } from "node:fs";
import { assertSynchronous } from "./store";
import type { AgentStore, StoreTransaction, Tables } from "./types";

const tableNames = {
  sessions: "lenso_agent_sessions",
  messages: "lenso_agent_messages",
  runs: "lenso_agent_runs",
  calls: "lenso_agent_calls",
  actions: "lenso_agent_actions",
} as const;
const schemaVersion = 1;

function requireSchema(db: Database): void {
  const versions = db.query<{ singleton: number; version: number }, []>(
    "SELECT singleton, version FROM lenso_agent_schema",
  ).all();
  if (versions.length !== 1 || versions[0].singleton !== 1 || versions[0].version !== schemaVersion)
    throw new Error("Unsupported agent database schema version");
  for (const name of Object.values(tableNames)) {
    const columns = db.query<{ name: string; type: string; notnull: number; pk: number }, []>(
      `PRAGMA table_info(${name})`,
    ).all();
    const expected = [
      ["sequence", "INTEGER", 0, 1],
      ["id", "TEXT", 1, 0],
      ["session_id", "TEXT", 1, 0],
      ["value", "TEXT", 1, 0],
    ];
    if (JSON.stringify(columns.map(column => [column.name, column.type, column.notnull, column.pk])) !==
      JSON.stringify(expected))
      throw new Error(`Invalid agent database table: ${name}`);
    const indexes = db.query<{ name: string; unique: number; partial: number }, []>(`PRAGMA index_list(${name})`).all();
    const indexColumns = (index: string) => db.query<{ name: string }, [string]>(
      `SELECT name FROM pragma_index_info(?) ORDER BY seqno`,
    ).all(index).map(column => column.name);
    if (!indexes.some(index => index.unique === 1 && index.partial === 0 &&
      JSON.stringify(indexColumns(index.name)) === '["id"]') ||
      !indexes.some(index => index.name === `${name}_session_id` && index.partial === 0 &&
        JSON.stringify(indexColumns(index.name)) === '["session_id"]'))
      throw new Error(`Invalid agent database indexes: ${name}`);
  }
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
      db.exec(readFileSync(new URL("../migrations/001-agent.sql", import.meta.url), "utf8"));
    } else if (!existing.some(row => row.name === "lenso_agent_schema")) {
      throw new Error("Agent database schema version is missing");
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
        put<K extends keyof Tables>(table: K, record: Tables[K]): void {
          check();
          const json = JSON.stringify(record);
          const value = JSON.parse(json) as Tables[K];
          const sessionId = "sessionId" in value ? value.sessionId : value.id;
          db.query(
            `INSERT INTO ${sqlName(table)} (id, session_id, value) VALUES (?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET session_id = excluded.session_id, value = excluded.value`,
          ).run(value.id, sessionId, json);
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
