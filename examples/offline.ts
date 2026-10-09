import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createFauxCore, fauxAssistantMessage, fauxToolCall, type FauxResponseStep } from "@earendil-works/pi-ai/providers/faux";
import type { TranscriptContext } from "@earendil-works/pi-ai";
import { createAgentService } from "../src/runtime";
import { createSqliteStore, migrateAgentDatabase } from "../src/sqlite";
import { createNotesHost, type Note } from "./notes";
import { consoleNotesProfile } from "./console-profile";
import { missingReceiptStore } from "./missing-receipt";

function latestResult(context: TranscriptContext, toolName: string): unknown {
  const message = context.messages.findLast(message => message.role === "toolResult" && message.toolName === toolName);
  assert(message?.role === "toolResult", `missing real ${toolName} result`);
  const text = message.content.filter(block => block.type === "text").map(block => block.text).join("");
  const value = JSON.parse(text);
  return value && typeof value === "object" && "result" in value ? value.result : value;
}

/** A real Pi loop with scripted provider responses, not a substitute Agent runtime. */
export async function offlineExample() {
  const directory = await mkdtemp(join(tmpdir(), "lenso-agent-offline-"));
  const filename = join(directory, "notes.sqlite");
  let host = await createNotesHost(filename);
  let agent: ReturnType<typeof createAgentService<Awaited<ReturnType<typeof host.contextFor>>>> | undefined;
  try {
    migrateAgentDatabase(host.db);
    const core = createFauxCore({ provider: "offline-notes", models: [{ id: "scripted-notes" }], tokensPerSecond: Infinity });
    const profile = consoleNotesProfile<Awaited<ReturnType<typeof host.contextFor>>>();
    const boot = () => {
      const borrowed = createSqliteStore(host.db);
      const fault = missingReceiptStore(borrowed);
      agent = createAgentService({
        store: fault.store, identity: host.identity, tools: host.tools,
        model: core.models[0], profiles: [profile],
      }, { streamFn: core.streamSimple });
      return fault;
    };
    let fault = boot();
    let context = await host.contextFor("alice");
    const session = await agent!.createSession(context, profile.id);
    const notes = host.running.get(host.notes);
    const forgedActor = { ...context.actor, audience: "notes:read" } as unknown as Parameters<typeof notes.read>[1]["actor"];
    await assert.rejects(() => notes.read({ id: "welcome" }, { actor: forgedActor, signal: new AbortController().signal }));
    const bob = await host.contextFor("bob");
    const bobIdentity = await host.identity(bob);
    const bobSignal = new AbortController().signal;
    await assert.rejects(() => host.tools.prepare("notes_update",
      { id: "welcome", title: "Unauthorized", body: "", expectedVersion: 1 }, bob, bobIdentity, bobSignal));
    const bobActor = await host.actorFor(bob, "read", bobSignal);
    await assert.rejects(() => notes.read({ id: "welcome" }, { actor: bobActor, signal: bobSignal }));
    console.log("real Auth rejected forged provenance and the service rejected another owner's read");
    async function run(responses: FauxResponseStep[], text: string) {
      core.setResponses(responses);
      const handle = await agent!.startRun(context, session.id, text);
      const final = await handle.done;
      assert.equal(final.status, "completed");
      return agent!.readSession(context, session.id);
    }
    async function propose(tool: "notes_update" | "notes_remove", title = "Reviewed") {
      const state = await run([
        fauxAssistantMessage(fauxToolCall("notes_read", { id: "welcome" })),
        transcript => {
          const fact = latestResult(transcript, "notes_read") as Note;
          assert.equal(fact.id, "welcome");
          assert.equal(typeof fact.version, "number");
          return fauxAssistantMessage(tool === "notes_update"
            ? fauxToolCall(tool, { id: fact.id, title, body: fact.body, expectedVersion: fact.version })
            : fauxToolCall(tool, { id: fact.id, expectedVersion: fact.version }));
        },
        fauxAssistantMessage("The exact write is pending authenticated confirmation, not executed."),
      ], `Read the note, then propose ${tool}.`);
      const action = state.actions.findLast(action => action.status === "pending");
      assert(action);
      console.log("pending exact write", action.prepared.input);
      return action;
    }

    const action = await propose("notes_update");
    assert.equal(host.db.query<Note, [string]>("SELECT * FROM notes WHERE id = ?").get("welcome")!.version, 1);
    const confirmed = await agent!.confirmAction(context, action.id, action.digest);
    assert.equal(confirmed.status, "completed");
    assert.equal((confirmed.result as Note).version, 2);
    console.log("authenticated confirmation, real result", confirmed.result);
    await run([
      fauxAssistantMessage(fauxToolCall("notes_read", { id: "welcome" })),
      transcript => {
        const result = latestResult(transcript, "notes_read") as Note;
        assert.deepEqual(result, confirmed.result);
        return fauxAssistantMessage(`Consumed the actual write result: "${result.title}", version ${result.version}.`);
      },
    ], "Read the note again and report the actual confirmation result.");
    console.log("consumed response recorded");

    const cancelled = await propose("notes_remove");
    assert.equal((await agent!.cancelAction(context, cancelled.id)).status, "cancelled");
    assert.equal(host.db.query<Note, [string]>("SELECT * FROM notes WHERE id = ?").get("welcome")!.version, 2);
    console.log("explicit cancellation left the note intact");

    const revoked = await propose("notes_update", "Must not happen");
    host.revoke(context);
    await assert.rejects(() => agent!.confirmAction(context, revoked.id, revoked.digest));
    assert.equal(host.db.query<Note, [string]>("SELECT * FROM notes WHERE id = ?").get("welcome")!.title, "Reviewed");
    console.log("revoked authority denied confirmation");

    context = await host.contextFor("alice");
    await agent!.cancelAction(context, revoked.id);
    const restartPending = await propose("notes_update", "After restart");
    const before = await agent!.readSession(context, session.id);
    await agent!.close();
    await host.close();
    host = await createNotesHost(filename);
    context = await host.contextFor("alice");
    fault = boot();
    const after = await agent!.readSession(context, session.id);
    assert.equal(after.messages.length, before.messages.length);
    assert.equal(after.actions.find(item => item.id === restartPending.id)?.status, "pending");
    assert.equal(host.db.query<Note, [string]>("SELECT * FROM notes WHERE id = ?").get("welcome")!.version, 2);
    assert.equal((await agent!.confirmAction(context, restartPending.id, restartPending.digest)).status, "completed");
    console.log("restart preserved history, pending action and durable business SQL");

    const ambiguous = await propose("notes_update", "Committed without receipt");
    fault.arm(ambiguous.id);
    assert.equal((await agent!.confirmAction(context, ambiguous.id, ambiguous.digest)).status, "executing");
    const committed = host.db.query<Note, [string]>("SELECT * FROM notes WHERE id = ?").get("welcome")!;
    assert.equal(committed.title, "Committed without receipt");
    assert.equal(committed.version, 4);
    assert.equal((await agent!.readSession(context, session.id)).actions.find(item => item.id === ambiguous.id)?.status, "executing");
    // Previous executor is fully drained before exclusive offline recovery.
    await agent!.close();
    await host.close();
    host = await createNotesHost(filename);
    context = await host.contextFor("alice");
    boot();
    await agent!.recoverInterrupted();
    const recovered = await agent!.readSession(context, session.id);
    assert.equal(recovered.actions.find(item => item.id === ambiguous.id)?.status, "outcome_unknown");
    assert.equal(host.db.query<Note, [string]>("SELECT * FROM notes WHERE id = ?").get("welcome")!.version, 4);
    await assert.rejects(() => agent!.confirmAction(context, ambiguous.id, ambiguous.digest));
    console.log("offline recovery: outcome_unknown; no side-effect replay");
  } finally {
    try { await agent?.close(); }
    finally {
      try { await host.close(); }
      finally { await rm(directory, { recursive: true, force: true }); }
    }
  }
}

if (import.meta.main) await offlineExample();
