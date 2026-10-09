import { Database } from "bun:sqlite";
import { audience, createAuth, defineSource, realm, type Actor, type ActorOf } from "@lenso/auth";
import { definePlugin, startApp } from "@lenso/core";
import { defineOperation } from "@lenso/engine/operations";
import { z } from "zod";
import { createManageToolSource } from "../src/manage";
import type { Identity } from "../src/types";

export const notesAudiences = {
  session: audience("agent:session"),
  read: audience("notes:read"),
  update: audience("notes:update"),
  remove: audience("notes:remove"),
} as const;

export interface Note {
  id: string;
  ownerId: string;
  title: string;
  body: string;
  version: number;
}
type BusinessActor = Actor<"notes", string, "notes:read" | "notes:update" | "notes:remove">;

/** Standalone version of Notes' operation audiences, provenance and owner policy.
 * https://github.com/LioRael/lenso/blob/main/examples/notes/src/notes.ts
 * Versioned SQL writes add atomic stale-proposal protection, not an Agent retry.
 */
export async function createNotesHost(filename: string) {
  const db = new Database(filename);
  const cleanup: (() => void | Promise<void>)[] = [() => db.close()];
  let closing: Promise<void> | undefined;
  const close = () => closing ??= (async () => {
    const failures: unknown[] = [];
    for (const dispose of cleanup) {
      try { await dispose(); } catch (error) { failures.push(error); }
    }
    if (failures.length) throw new AggregateError(failures, "Notes host cleanup failed");
  })();
  try {
    db.exec("CREATE TABLE IF NOT EXISTS notes (id TEXT PRIMARY KEY, ownerId TEXT NOT NULL, title TEXT NOT NULL, body TEXT NOT NULL, version INTEGER NOT NULL)");
    db.query("INSERT OR IGNORE INTO notes VALUES (?, ?, ?, ?, ?)").run("welcome", "alice", "Welcome", "Read this fact before proposing a write.", 1);
    // These are opaque offline evidence handles, not bearer tokens accepted from JSON.
    const evidence = new WeakMap<object, { subject: string; active: boolean }>();
    const source = defineSource<object>({
      async verify(handle, { signal }) {
        signal.throwIfAborted();
        const record = evidence.get(handle);
        return record?.active ? { status: "verified", subjectId: record.subject, kind: "user" } : { status: "rejected" };
      },
    });
    const authentication = createAuth(realm("notes", source));
    cleanup.unshift(() => authentication.close());
    const sessionAccess = authentication.for(notesAudiences.session);
    type SessionActor = ActorOf<typeof sessionAccess>;
    const actorEvidence = new WeakMap<SessionActor, object>();
    type Context = { actor: SessionActor };
    const contextFor = async (subject: string): Promise<Context> => {
      const handle = {};
      evidence.set(handle, { subject, active: true });
      const actor = await sessionAccess.required(handle);
      actorEvidence.set(actor, handle);
      return { actor };
    };
    const revoke = (context: Context) => {
      const handle = actorEvidence.get(context.actor);
      const record = handle && evidence.get(handle);
      if (record) record.active = false;
    };
    const read = (id: string) => db.query<Note, [string]>("SELECT * FROM notes WHERE id = ?").get(id);
    const database = definePlugin({ id: "notes-database", setup: () => db });
    const authPlugin = definePlugin({ id: "notes-auth", setup: () => authentication });
    const lookup = z.object({ id: z.string().min(1).max(128) }).strict();
    const update = lookup.extend({
      title: z.string().min(1).max(256),
      body: z.string().max(8192),
      expectedVersion: z.number().int().positive(),
    }).strict();
    const remove = lookup.extend({ expectedVersion: z.number().int().positive() }).strict();
    type BusinessContext = { actor: BusinessActor; signal: AbortSignal };
    const notes = definePlugin({
      id: "notes",
      requires: [database, authPlugin],
      setup(lifecycle) {
        lifecycle.get(database);
        const auth = lifecycle.get(authPlugin);
        async function owned(operation: "read" | "update" | "remove", id: string, context: BusinessContext) {
          const access = auth.for(notesAudiences[operation]);
          await access.enforce(context.actor, undefined as Note | undefined,
            ({ principal }) => principal.kind === "user", { signal: context.signal });
          const note = read(id);
          if (note) await access.enforce(context.actor, note,
            ({ principal, resource }) => resource?.ownerId === principal.subjectId, { signal: context.signal });
          return note;
        }
        return {
          async read(input: z.infer<typeof lookup>, context: BusinessContext) {
            return owned("read", input.id, context);
          },
          async update(input: z.infer<typeof update>, context: BusinessContext) {
            const parsed = update.parse(input);
            const existing = await owned("update", parsed.id, context);
            if (!existing) return null;
            context.signal.throwIfAborted();
            const result = db.query("UPDATE notes SET title = ?, body = ?, version = version + 1 WHERE id = ? AND ownerId = ? AND version = ?")
              .run(parsed.title, parsed.body, parsed.id, context.actor.subjectId, parsed.expectedVersion);
            if (result.changes !== 1) throw new Error("stale-note");
            return read(parsed.id);
          },
          async remove(input: z.infer<typeof remove>, context: BusinessContext) {
            const parsed = remove.parse(input);
            const existing = await owned("remove", parsed.id, context);
            if (!existing) return { removed: false };
            context.signal.throwIfAborted();
            const result = db.query("DELETE FROM notes WHERE id = ? AND ownerId = ? AND version = ?")
              .run(parsed.id, context.actor.subjectId, parsed.expectedVersion);
            if (result.changes !== 1) throw new Error("stale-note");
            return { removed: true };
          },
        };
      },
    });
    const operations = [
      defineOperation({ plugin: notes, method: "read", input: lookup, context: true, effect: "read", description: "Read an owned note and its current version." }),
      defineOperation({ plugin: notes, method: "update", input: update, context: true, effect: "write", description: "Update an owned note only at the expected version." }),
      defineOperation({ plugin: notes, method: "remove", input: remove, context: true, effect: "write", destructive: true, description: "Remove an owned note only at the expected version." }),
    ];
    const plugins = [database, authPlugin, notes];
    const running = await startApp({ instanceId: "offline-notes", plugins });
    cleanup.unshift(() => running.stop());
    const identity = async (context: Context): Promise<Identity> => {
      const actor = await sessionAccess.enforce(context.actor, undefined, ({ principal }) => principal.kind === "user");
      return { subject: actor.subjectId, scope: `owner:${actor.subjectId}`, application: "notes-demo", target: running.instanceId };
    };
    const actorFor = async (context: Context, operation: "read" | "update" | "remove", signal: AbortSignal) => {
      await identity(context);
      const handle = actorEvidence.get(context.actor);
      if (!handle) throw new Error("unauthenticated");
      return authentication.for(notesAudiences[operation]).required(handle, { signal });
    };
    const tools = createManageToolSource<Context>({
      running, application: "notes-demo", plugins,
      selections: operations.map(operation => ({ name: `notes_${operation.method}`, operation })),
      async authorize(_operation, input, context, claimed) {
        const current = await identity(context);
        if (current.subject !== claimed.subject || current.scope !== claimed.scope) return false;
        const note = input === undefined ? undefined : read((input as { id: string }).id);
        return !note || note.ownerId === current.subject;
      },
      async binding(operation, _validatedInput, context, claimed, signal) {
        await identity(context);
        if (context.actor.subjectId !== claimed.subject) throw new Error("unauthenticated");
        const actor = await actorFor(context, operation.method as "read" | "update" | "remove", signal);
        return { context: { actor, signal }, signal };
      },
      async precondition(operation, input) {
        if (operation.effect === "read") return null;
        const note = read((input as { id: string }).id);
        return note ? `${note.id}:${note.version}` : "absent";
      },
    });
    return {
      db, running, notes, operations, tools, identity, contextFor, revoke, actorFor, close,
    };
  } catch (error) {
    try { await close(); }
    catch (cleanupError) { throw new AggregateError([error, cleanupError], "Notes host startup failed"); }
    throw error;
  }
}
