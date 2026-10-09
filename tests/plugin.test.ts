import { expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { definePlugin, startApp } from "@lenso/core";
import { createFauxCore, fauxAssistantMessage } from "@earendil-works/pi-ai/providers/faux";
import { createAgentPlugin } from "../src/plugin";
import { createSqliteStore, migrateAgentDatabase } from "../src/sqlite";
import type { AgentService } from "../src/types";

// Prevent shutdown from disposing host-owned DBs or leaving an owned Pi run
// alive. Runtime close tests alone cannot establish plugin cleanup ordering.
test("plugin stop drains Agent before dependency cleanup and keeps borrowed SQLite alive", async () => {
  const db = new Database(":memory:");
  migrateAgentDatabase(db);
  const core = createFauxCore({ provider: "plugin-offline", tokensPerSecond: 1 });
  core.setResponses([fauxAssistantMessage("A deliberately slow response to cancel.")]);
  let finished = false;
  let dependencySawDrained = false;
  const database = definePlugin({
    id: "borrowed-database",
    setup(context) {
      context.onCleanup(() => { dependencySawDrained = finished; });
      return createSqliteStore(db);
    },
  });
  const plugin = createAgentPlugin<{}>({
    id: "notes-agent", requires: [database], pi: { streamFn: core.streamSimple },
    options(context) {
      return {
        store: context.get(database),
        model: core.models[0],
        identity: async () => ({ subject: "alice", scope: "owner:alice", application: "notes", target: context.instanceId }),
        tools: { catalog: async () => [], prepare: async () => { throw new Error("no-tools"); }, invoke: async () => { throw new Error("no-tools"); } },
        profiles: [{ id: "notes", instructions: "Read only.", tools: [] }],
      };
    },
  });
  const app = await startApp({ instanceId: "plugin-host", plugins: [database, plugin] });
  try {
    const agent = app.get(plugin);
    const session = await agent.createSession({}, "notes");
    const handle = await agent.startRun({}, session.id, "Wait.");
    void handle.done.then(() => { finished = true; });
    await app.stop();
    expect((await handle.done).status).toBe("cancelled");
    expect(dependencySawDrained).toBe(true);
    expect(db.query<{ value: number }, []>("SELECT 42 AS value").get()!.value).toBe(42);
  } finally {
    await app.stop();
    db.close();
  }
});

// Failed later setup must unwind Agent even though the caller never receives
// RunningApp. This catches cleanup registered too late in plugin setup.
test("failed downstream setup closes Agent while borrowed DB remains usable", async () => {
  const db = new Database(":memory:");
  migrateAgentDatabase(db);
  let agent: AgentService<{}> | undefined;
  let sessionId = "";
  const core = createFauxCore({ provider: "plugin-failure" });
  const database = definePlugin({ id: "borrowed", setup: () => createSqliteStore(db) });
  const plugin = createAgentPlugin<{}>({
    id: "agent", requires: [database], pi: { streamFn: core.streamSimple },
    options(context) {
      return {
        store: context.get(database), model: core.models[0],
        identity: async () => ({ subject: "alice", scope: "owner:alice", application: "notes", target: context.instanceId }),
        profiles: [{ id: "notes", instructions: "Read only.", tools: [] }],
        tools: { catalog: async () => [], prepare: async () => { throw new Error("no-tools"); }, invoke: async () => { throw new Error("no-tools"); } },
      };
    },
  });
  const failing = definePlugin({
    id: "fails-later", requires: [plugin],
    async setup(context) {
      agent = context.get(plugin);
      sessionId = (await agent.createSession({}, "notes")).id;
      throw new Error("expected-setup-failure");
    },
  });
  try {
    await expect(startApp({ plugins: [database, plugin, failing] })).rejects.toThrow("expected-setup-failure");
    expect(agent).toBeDefined();
    await expect(agent!.startRun({}, sessionId, "No run after cleanup.")).rejects.toThrow();
    expect(db.query<{ value: number }, []>("SELECT 1 AS value").get()!.value).toBe(1);
  } finally { db.close(); }
});
