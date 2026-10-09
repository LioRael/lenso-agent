import { afterEach, expect, test } from "bun:test";
import { defineApp, definePlugin, startApp, type RunningApp } from "@lenso/core";
import { defineOperation } from "@lenso/engine/operations";
import { z } from "zod";
import { createManageToolSource, type ManageToolSourceOptions } from "../src/manage";
import { AgentError } from "../src/safety";
import type { Identity, PreparedTool } from "../src/types";

interface HostContext { subject: string; scope: string }
interface BusinessContext { actor: string; scope: string; signal: AbortSignal }
const context: HostContext = { subject: "alice", scope: "tenant-a" };
const signal = () => new AbortController().signal;
const apps: RunningApp[] = [];
afterEach(async () => {
  await Promise.all(apps.splice(0).map((app) => app.stop()));
});

async function fixture() {
  const state = {
    allowed: true, version: "v1", calls: 0,
    bindingInput: undefined as unknown,
    business: undefined as BusinessContext | undefined,
    entrySignal: undefined as AbortSignal | undefined,
  };
  const input = z.object({
    amount: z.string().regex(/^\d+$/).transform(Number),
    actor: z.string().optional(),
    confirmed: z.boolean().optional(),
  });
  const plugin = definePlugin({
    id: "ledger",
    setup: ({ instanceId }) => {
      const service = {
        instanceId,
        async read() { return { instance: this.instanceId }; },
        async change(value: z.output<typeof input>, business: BusinessContext) {
          state.calls++;
          state.business = business;
          return { amount: value.amount, actor: business.actor, scope: business.scope, instance: this.instanceId };
        },
        async approved(value: z.output<typeof input>, business: BusinessContext) {
          return this.change(value, business);
        },
        async wait(_value: z.output<typeof input>, business: BusinessContext) {
          state.calls++;
          state.business = business;
          await new Promise<void>((resolve) => business.signal.addEventListener("abort", () => resolve(), { once: true }));
          return { cancelled: business.signal.aborted };
        },
        async fail() { throw new Error("private-domain-message-token"); },
        async hidden() { state.calls++; return {}; },
      };
      return service;
    },
  });
  const running = await startApp(defineApp({ plugins: [plugin], instanceId: "business-one" }));
  apps.push(running);
  const read = defineOperation({ plugin, method: "read", description: "Read ledger", input: z.object({}), effect: "read" });
  const change = defineOperation({ plugin, method: "change", description: "Change ledger", input, context: true, effect: "write" });
  const approved = defineOperation({
    plugin, method: "approved", description: "Approved ledger change", input, context: true,
    effect: "read", confirmation: "required", approval: "required",
  });
  const wait = defineOperation({ plugin, method: "wait", description: "Wait for cancellation", input, context: true, effect: "read" });
  const fail = defineOperation({ plugin, method: "fail", description: "Fail safely", input: z.object({}), effect: "read" });
  const identity: Identity = { ...context, application: "accounts", target: running.instanceId };
  const options: ManageToolSourceOptions<HostContext> = {
    running, application: "accounts", plugins: [plugin],
    selections: [{ name: "read_ledger", operation: read }, { name: "change_ledger", operation: change }],
    authorize: async (_operation, _input, host, actor) =>
      state.allowed && actor.subject === host.subject && actor.scope === host.scope,
    binding: async (_operation, validated, _host, actor, entrySignal) => {
      state.bindingInput = validated;
      state.entrySignal = entrySignal;
      return {
        context: { actor: actor.subject, scope: actor.scope, signal: entrySignal },
        signal: entrySignal,
      };
    },
    precondition: async () => state.version,
  };
  const source = (overrides: Partial<ManageToolSourceOptions<HostContext>> = {}) =>
    createManageToolSource({ ...options, ...overrides });
  return { state, plugin, running, read, change, approved, wait, fail, identity, options, source };
}

async function expectCode(work: Promise<unknown>, code: string) {
  try {
    await work;
    throw new Error(`Expected ${code}`);
  } catch (error) {
    expect(error).toBeInstanceOf(AgentError);
    expect((error as AgentError).code).toBe(code);
    expect((error as Error).message).toBe(code);
    expect((error as Error).cause).toBeUndefined();
  }
}

test("catalog projects shared object schemas under stable aliases and the exact running target", async () => {
  const f = await fixture();
  const source = f.source({ authorize: async (operation) => operation === f.change });
  const catalog = await source.catalog(context, f.identity, signal());
  expect(catalog.map((tool) => tool.name)).toEqual(["change_ledger"]);
  expect(catalog[0]!.target).toEqual({
    application: "accounts", instance: "business-one", plugin: "ledger", operation: "change",
  });
  expect(catalog[0]!.inputSchema.type).toBe("object");
  expect((catalog[0]!.inputSchema.properties as Record<string, { type: string }>).amount!.type).toBe("string");
  expect(catalog[0]!.confirmation).toBe(true);
  const unknown = f.source({ selections: [{ name: "unknown_change", operation: { ...f.change, effect: undefined } }] });
  expect((await unknown.catalog(context, f.identity, signal()))[0]!.confirmation).toBe(true);
  const reordered = f.source({ selections: [...f.options.selections].reverse() });
  expect((await reordered.catalog(context, f.identity, signal())).map((tool) => tool.name))
    .toEqual(["change_ledger", "read_ledger"]);
  const prepared = await reordered.prepare("read_ledger", {}, context, f.identity, signal());
  expect(await reordered.invoke(prepared, context, f.identity, signal(), false)).toEqual({ instance: "business-one" });
});

test("the source rejects same-ID substitute plugins, duplicate aliases and undeclared names", async () => {
  const f = await fixture();
  const impostor = { ...f.plugin };
  expect(() => f.source({
    plugins: [impostor], selections: [{ name: "read_ledger", operation: { ...f.read, plugin: impostor } }],
  })).toThrow();
  expect(() => f.source({ selections: [
    { name: "read_ledger", operation: f.read }, { name: "read_ledger", operation: f.change },
  ] })).toThrow("invalid-tool-source");
  const source = f.source();
  await expectCode(source.prepare("hidden", {}, context, f.identity, signal()), "unknown-operation");
  await expectCode(source.prepare("operation_0", {}, context, f.identity, signal()), "unknown-operation");
  expect(f.state.calls).toBe(0);
});

test("identity is host-bound, never actor or confirmation fields from model JSON", async () => {
  const f = await fixture();
  const source = f.source();
  for (const identity of [
    { ...f.identity, subject: "mallory" }, { ...f.identity, scope: "tenant-b" },
    { ...f.identity, application: "other" }, { ...f.identity, target: "business-two" },
  ]) {
    if (identity.application !== "accounts" || identity.target !== "business-one")
      await expectCode(source.catalog(context, identity, signal()), "forbidden-operation");
    else expect(await source.catalog(context, identity, signal())).toEqual([]);
    await expectCode(source.prepare("change_ledger", { amount: "7" }, context, identity, signal()), "forbidden-operation");
  }
  const prepared = await source.prepare("change_ledger", { amount: "7", actor: "mallory", confirmed: true }, context, f.identity, signal());
  await expectCode(source.invoke(prepared, context, f.identity, signal(), false), "confirmation-required");
  expect(await source.invoke(prepared, context, f.identity, signal(), true)).toEqual({
    amount: 7, actor: "alice", scope: "tenant-a", instance: "business-one",
  });
});

test("revocation after catalog or preparation stops dispatch", async () => {
  const f = await fixture();
  const source = f.source();
  expect(await source.catalog(context, f.identity, signal())).toHaveLength(2);
  const prepared = await source.prepare("change_ledger", { amount: "7" }, context, f.identity, signal());
  f.state.allowed = false;
  await expectCode(source.prepare("change_ledger", { amount: "7" }, context, f.identity, signal()), "forbidden-operation");
  await expectCode(source.invoke(prepared, context, f.identity, signal(), true), "forbidden-operation");
  let validatedChecks = 0;
  const racing = f.source({
    authorize: async (_operation, input) => input === undefined || ++validatedChecks < 2,
  });
  await expectCode(racing.invoke(prepared, context, f.identity, signal(), true), "forbidden-operation");
  expect(f.state.calls).toBe(0);
});

test("raw input survives transforms and host binding gets validated data with method this intact", async () => {
  const f = await fixture();
  const source = f.source();
  const raw = { amount: "007" };
  const entrySignal = signal();
  const prepared = await source.prepare("change_ledger", raw, context, f.identity, entrySignal);
  expect(prepared.input).toEqual({ amount: "007" });
  raw.amount = "999";
  const result = await source.invoke(prepared, context, f.identity, entrySignal, true);
  expect(result).toEqual({ amount: 7, actor: "alice", scope: "tenant-a", instance: "business-one" });
  expect(f.state.bindingInput).toEqual({ amount: 7 });
  expect(f.state.entrySignal).toBe(entrySignal);
  expect(f.state.business!.signal).toBe(entrySignal);
  await expectCode(source.prepare("change_ledger", { amount: 7 }, context, f.identity, signal()), "invalid-input");
});

test("every prepared definition field is checked before business invocation", async () => {
  const f = await fixture();
  const source = f.source();
  const prepared = await source.prepare("change_ledger", { amount: "7" }, context, f.identity, signal());
  const mutations: ((copy: PreparedTool) => void)[] = [
    (copy) => { copy.definition.description = "changed"; },
    (copy) => { copy.definition.inputSchema = { type: "object" }; },
    (copy) => { copy.definition.target.application = "other"; },
    (copy) => { copy.definition.target.instance = "other"; },
    (copy) => { copy.definition.target.plugin = "other"; },
    (copy) => { copy.definition.target.operation = "hidden"; },
    (copy) => { copy.definition.effect = "read"; },
    (copy) => { copy.definition.confirmation = false; },
    (copy) => { Object.assign(copy.definition, { extra: "unapproved metadata" }); },
    (copy) => { Object.assign(copy, { extra: "unapproved preparation" }); },
  ];
  for (const mutate of mutations) {
    const copy = structuredClone(prepared);
    mutate(copy);
    await expectCode(source.invoke(copy, context, f.identity, signal(), true), "tool-conflict");
  }
  Object.assign(f.change, { description: "Changed live metadata" });
  await expectCode(source.invoke(prepared, context, f.identity, signal(), true), "tool-conflict");
  expect(f.state.calls).toBe(0);
});

test("precondition changes reject preparation replay, including changes during adapter validation", async () => {
  const f = await fixture();
  const source = f.source();
  const prepared = await source.prepare("change_ledger", { amount: "7" }, context, f.identity, signal());
  f.state.version = "v2";
  await expectCode(source.invoke(prepared, context, f.identity, signal(), true), "precondition-conflict");
  f.state.version = "v1";
  let validatedChecks = 0;
  const racing = f.source({
    authorize: async (_operation, input) => {
      if (input !== undefined && ++validatedChecks === 2) f.state.version = "v2";
      return true;
    },
  });
  await expectCode(racing.invoke(prepared, context, f.identity, signal(), true), "precondition-conflict");
  expect(f.state.calls).toBe(0);
});

test("required confirmation cannot be disabled and gateway confirmation never bypasses host approval", async () => {
  const f = await fixture();
  let approved = false;
  let hostConfirmCalls = 0;
  const source = f.source({
    selections: [{ name: "approved_change", operation: f.approved, confirmation: false }],
    binding: async (...args) => ({
      ...await f.options.binding(...args),
      confirm: () => { hostConfirmCalls++; return true; },
      approve: () => approved,
    }),
  });
  const prepared = await source.prepare("approved_change", { amount: "7" }, context, f.identity, signal());
  expect(prepared.definition.confirmation).toBe(true);
  await expectCode(source.invoke(prepared, context, f.identity, signal(), false), "confirmation-required");
  await expectCode(source.invoke(prepared, context, f.identity, signal(), true), "approval-required");
  approved = true;
  await source.invoke(prepared, context, f.identity, signal(), true);
  expect(hostConfirmCalls).toBe(0);
  expect(f.state.calls).toBe(1);
});

test("abort reaches the business context and entry even if binding supplies a different entry signal", async () => {
  const f = await fixture();
  const controller = new AbortController();
  const source = f.source({
    selections: [{ name: "wait_ledger", operation: f.wait }],
    binding: async (...args) => ({ ...await f.options.binding(...args), signal: signal() }),
  });
  const prepared = await source.prepare("wait_ledger", { amount: "7" }, context, f.identity, controller.signal);
  const invoked = source.invoke(prepared, context, f.identity, controller.signal, false);
  while (!f.state.business) await Bun.sleep(1);
  controller.abort(new Error("private-abort-reason"));
  await expectCode(invoked, "cancelled");
  expect(f.state.business.signal).toBe(controller.signal);
  await expectCode(source.prepare("wait_ledger", { amount: "7" }, context, f.identity, controller.signal), "cancelled");
});

test("invalid and oversized JSON and raw business failures expose codes only", async () => {
  const f = await fixture();
  const source = f.source({ selections: [{ name: "fail_ledger", operation: f.fail }] });
  const prepared = await source.prepare("fail_ledger", {}, context, f.identity, signal());
  await expectCode(source.invoke(prepared, context, f.identity, signal(), false), "operation-failed");
  Object.assign(f.fail, {
    mapError: () => ({ code: "private-domain-token", phase: "invoke", message: "private-domain-message" }),
  });
  await expectCode(source.invoke(prepared, context, f.identity, signal(), false), "operation-failed");
  Object.assign(f.fail, {
    mapError: () => ({ code: "invalid-input", phase: "invoke", message: "private-domain-message" }),
  });
  await expectCode(source.invoke(prepared, context, f.identity, signal(), false), "invalid-input");
  await expectCode(source.prepare("fail_ledger", { amount: Infinity }, context, f.identity, signal()), "operation-failed");
  await expectCode(source.prepare("fail_ledger", { payload: "x".repeat(1024 * 1024) }, context, f.identity, signal()), "operation-failed");
  const cyclic: Record<string, unknown> = {};
  cyclic.self = cyclic;
  await expectCode(source.prepare("fail_ledger", cyclic, context, f.identity, signal()), "operation-failed");
  const scalar = f.source({ selections: [{ name: "scalar_read", operation: { ...f.read, input: z.string() } }] });
  await expectCode(scalar.catalog(context, f.identity, signal()), "operation-failed");
});
