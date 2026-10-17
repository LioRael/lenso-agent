// Runs only against the parent test's disposable SQLite file; no network or credentials.
import { Database } from "bun:sqlite";
import { createFauxCore, fauxAssistantMessage, fauxToolCall } from "@earendil-works/pi-ai";
import { createAgentService } from "../../src/runtime";
import { createSqliteStore, migrateAgentDatabase } from "../../src/sqlite";
import type { ToolDefinition } from "../../src/types";
const identity = { subject: "alice", scope: "test", application: "local", target: "fixture" };
const db = new Database(process.argv[2]);
migrateAgentDatabase(db);
db.exec("CREATE TABLE fixture_effects (id INTEGER PRIMARY KEY)");
const definition: ToolDefinition = {
  name: "local_effect", description: "Disposable local fixture", inputSchema: { type: "object" },
  target: { application: "local", instance: "fixture", plugin: "test", operation: "local_effect" },
  effect: "read", confirmation: false,
};
let entered!: () => void;
const ready = new Promise<void>(resolve => { entered = resolve; });
const core = createFauxCore({ models: [{ id: "offline" }] });
core.setResponses([fauxAssistantMessage(fauxToolCall("local_effect", {}))]);
const service = createAgentService({
  store: createSqliteStore(db), model: core.getModel(), identity: async () => identity,
  profiles: [{ id: "default", instructions: "Offline", tools: [definition.name] }],
  tools: {
    async catalog() { return [definition]; },
    async prepare(_name, input) { return { definition, input, precondition: null }; },
    async invoke() {
      db.exec("INSERT INTO fixture_effects DEFAULT VALUES");
      entered();
      await new Promise(() => {}); // Deliberately uncooperative: the parent kills this process.
    },
  },
}, { streamFn: core.streamSimple });
const session = await service.createSession(identity, "default");
const handle = await service.startRun(identity, session.id, "One local effect", { requestId: "kill-test" });
await ready;
await service.requestCancelRun(identity, handle.run.id);
console.log(JSON.stringify({ sessionId: session.id, runId: handle.run.id }));
setInterval(() => {}, 1000);
await handle.done;
