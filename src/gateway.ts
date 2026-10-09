import { createHash } from "node:crypto";
import { stableJson } from "@lenso/engine/diagnostics";
import {
  AgentError, assertSafeInput, jsonCopy, sameIdentity, safeCode, safeValue,
} from "./safety";
import { persistActionOutcome } from "./outcomes";
import type {
  AgentOptions, EventKind, Identity, PendingAction, PreparedTool,
  Run, RunLimits, Session, StoreTransaction, ToolCall,
} from "./types";

export type GatewayEmit = (
  kind: EventKind,
  details?: { callId?: string; actionId?: string; data?: unknown },
) => void;

function actionDigest(action: PendingAction): string {
  const { id, callId, sessionId, runId, identity, prepared, createdAt, expiresAt } = action;
  return createHash("sha256").update(stableJson({
    id, callId, sessionId, runId, identity, prepared, createdAt, expiresAt,
  })).digest("hex");
}

export function createExecutionGateway<C>(options: AgentOptions<C>, limits: RunLimits) {
  const { store, tools } = options;
  const secrets = () => options.secrets?.() ?? [];
  const persist = <T>(work: (tx: StoreTransaction) => T): T => store.transaction(work);

  async function identityFor(context: C, expected: Identity): Promise<Identity> {
    const resolved = await options.identity(context);
    const identity: Identity = {
      subject: resolved.subject, scope: resolved.scope,
      application: resolved.application, target: resolved.target,
    };
    for (const key of ["subject", "scope", "application", "target"] as const) {
      if (typeof identity[key] !== "string" || !identity[key] || identity[key].length > 512)
        throw new AgentError("invalid-identity");
    }
    assertSafeInput(identity, secrets(), 16_384);
    if (!sameIdentity(identity, expected)) throw new AgentError("forbidden");
    return identity;
  }
  function checkProfile(session: Session, name: string): void {
    const profile = options.profiles.find((item) => item.id === session.profile);
    if (!profile?.tools.includes(name)) throw new AgentError("tool-not-open");
  }
  function checkPrepared(prepared: PreparedTool, identity: Identity): void {
    if (prepared.definition.target.application !== identity.application ||
      prepared.definition.target.instance !== identity.target)
      throw new AgentError("target-mismatch");
    assertSafeInput(prepared, secrets(), limits.maxInputBytes + limits.maxContextBytes);
  }
  function getAction(id: string): PendingAction {
    const action = persist((tx) => tx.get("actions", id));
    if (!action) throw new AgentError("not-found");
    return action;
  }
  function getSession(id: string): Session {
    const session = persist((tx) => tx.get("sessions", id));
    if (!session) throw new AgentError("not-found");
    return session;
  }
  function actionUnavailable(action: PendingAction): never {
    throw new AgentError(`action-${action.status}`);
  }

  async function execute(
    context: C,
    call: ToolCall,
    signal: AbortSignal,
    action?: PendingAction,
    emit: GatewayEmit = () => {},
  ): Promise<unknown> {
    let enteredInvocation = false;
    try {
      const identity = await identityFor(context, call.identity);
      signal.throwIfAborted();
      await options.audit?.({
        kind: "tool_execution_admitted",
        sessionId: call.sessionId, runId: call.runId, callId: call.id,
        ...(action ? { actionId: action.id } : {}),
      });
      signal.throwIfAborted();
      // The durable executing marker precedes the effect. A missing receipt is not a retry grant.
      enteredInvocation = true;
      emit("tool_executing", { callId: call.id, actionId: action?.id });
      const raw = await tools.invoke(call.prepared, context, identity, signal, action !== undefined);
      const validated = jsonCopy(raw, limits.maxOutputBytes);
      const result = jsonCopy(safeValue(validated, secrets()), limits.maxOutputBytes);
      persist((tx) => {
        const current = tx.get("calls", call.id);
        if (!current || current.status !== "executing") throw new AgentError("execution-state-conflict");
        tx.put("calls", { ...current, status: "completed", result });
        if (action) {
          const pending = tx.get("actions", action.id);
          if (!pending || pending.status !== "executing") throw new AgentError("execution-state-conflict");
          const completed: PendingAction = { ...pending, status: "completed", result };
          tx.put("actions", completed);
          persistActionOutcome(tx, completed, secrets(), limits.maxOutputBytes);
        }
      });
      emit("tool_result", { callId: call.id, actionId: action?.id, data: result });
      return result;
    } catch (error) {
      const code = enteredInvocation ? "outcome-unknown" : safeCode(error);
      // A cancelled caller has not rolled back a business effect.
      try {
        persist((tx) => {
          const current = tx.get("calls", call.id);
          if (current?.status === "executing")
            tx.put("calls", {
              ...current,
              status: enteredInvocation ? "outcome_unknown" : "rejected",
              error: code,
            });
          if (action) {
            const pending = tx.get("actions", action.id);
            if (pending?.status === "executing") {
              const failed: PendingAction = {
                ...pending,
                status: enteredInvocation ? "outcome_unknown" : "rejected",
                error: code,
              };
              tx.put("actions", failed);
              persistActionOutcome(tx, failed, secrets(), limits.maxOutputBytes);
            }
          }
        });
      } catch {
        // Offline recovery will classify the remaining executing marker as unknown.
      }
      emit("safe_error", { callId: call.id, actionId: action?.id, data: { code } });
      throw new AgentError(code);
    }
  }

  return {
    async propose(
      context: C, session: Session, run: Run, modelCallId: string,
      name: string, input: unknown, signal: AbortSignal, emit: GatewayEmit,
    ): Promise<unknown> {
      signal.throwIfAborted();
      checkProfile(session, name);
      if (typeof modelCallId !== "string" || !modelCallId || modelCallId.length > 512)
        throw new AgentError("invalid-tool-call");
      assertSafeInput(modelCallId, secrets(), 16_384);
      assertSafeInput(input, secrets(), limits.maxInputBytes);
      const identity = await identityFor(context, session.identity);
      const prepared = jsonCopy(await tools.prepare(
        name, jsonCopy(input, limits.maxInputBytes), context, identity, signal,
      ));
      checkPrepared(prepared, identity);
      if (prepared.definition.name !== name) throw new AgentError("tool-mismatch");
      await identityFor(context, identity);
      signal.throwIfAborted();
      const now = Date.now();
      const call: ToolCall = {
        id: crypto.randomUUID(), sessionId: session.id, runId: run.id,
        modelCallId, identity, prepared, createdAt: now,
        status: prepared.definition.confirmation || prepared.definition.effect !== "read"
          ? "awaiting_confirmation" : "executing",
      };
      let action: PendingAction | undefined;
      if (call.status === "awaiting_confirmation") {
        action = {
          id: crypto.randomUUID(), callId: call.id, sessionId: session.id, runId: run.id,
          identity, prepared, digest: "",
          createdAt: now, expiresAt: now + limits.actionTtlMs, status: "pending",
        };
        action.digest = actionDigest(action);
        call.actionId = action.id;
      }
      persist((tx) => {
        if (tx.get("runs", run.id)?.status !== "running")
          throw new AgentError("run-not-running");
        if (tx.list("calls", session.id).some((item) =>
          item.runId === run.id && item.modelCallId === modelCallId))
          throw new AgentError("duplicate-tool-call");
        tx.put("calls", call);
        if (action) tx.put("actions", action);
      });
      emit("tool_prepared", { callId: call.id, actionId: action?.id });
      if (action) {
        emit("pending_action", { callId: call.id, actionId: action.id, data: action });
        await options.audit?.({
          kind: "action_proposed", sessionId: session.id, runId: run.id,
          callId: call.id, actionId: action.id,
        });
        // No suspended promise or model-controlled confirmation capability.
        return { status: "pending_confirmation", actionId: action.id };
      }
      return execute(context, call, signal, undefined, emit);
    },

    async confirm(context: C, actionId: string, digest: string, signal: AbortSignal): Promise<PendingAction> {
      const action = getAction(actionId);
      const identity = await identityFor(context, action.identity);
      const session = getSession(action.sessionId);
      checkProfile(session, action.prepared.definition.name);
      if (action.status !== "pending") actionUnavailable(action);
      if (digest !== action.digest || actionDigest(action) !== action.digest)
        throw new AgentError("confirmation-mismatch");
      if (action.expiresAt <= Date.now()) {
        persist((tx) => {
          const current = tx.get("actions", actionId);
          if (current?.status === "pending") {
            const expired: PendingAction = { ...current, status: "expired" };
            tx.put("actions", expired);
            const call = tx.get("calls", current.callId);
            if (call) tx.put("calls", { ...call, status: "rejected", error: "action-expired" });
            persistActionOutcome(tx, expired, secrets(), limits.maxOutputBytes);
          }
        });
        throw new AgentError("action-expired");
      }
      assertSafeInput(action.prepared.input, secrets(), limits.maxInputBytes);
      const current = await tools.prepare(
        action.prepared.definition.name, action.prepared.input, context, identity, signal,
      );
      checkPrepared(current, identity);
      if (stableJson(current) !== stableJson(action.prepared))
        throw new AgentError("precondition-conflict");
      await identityFor(context, identity);
      signal.throwIfAborted();
      const call = persist((tx) => {
        const pending = tx.get("actions", actionId);
        if (!pending) throw new AgentError("not-found");
        if (pending.status !== "pending") actionUnavailable(pending);
        if (pending.digest !== digest || stableJson(pending) !== stableJson(action))
          throw new AgentError("confirmation-mismatch");
        if (pending.expiresAt <= Date.now()) throw new AgentError("action-expired");
        if (tx.count("runs", { sessionId: session.id, status: "running" }) ||
          tx.count("calls", { sessionId: session.id, status: "executing" }))
          throw new AgentError("session-busy");
        const active = tx.count("runs", { status: "running" }) +
          tx.count("actions", { status: "executing" });
        if (active >= limits.maxConcurrentRuns) throw new AgentError("concurrency-budget");
        const storedCall = tx.get("calls", pending.callId);
        if (!storedCall || storedCall.status !== "awaiting_confirmation")
          throw new AgentError("execution-state-conflict");
        if (storedCall.actionId !== pending.id ||
          storedCall.runId !== pending.runId || storedCall.sessionId !== pending.sessionId ||
          !sameIdentity(storedCall.identity, pending.identity) ||
          stableJson(storedCall.prepared) !== stableJson(pending.prepared))
          throw new AgentError("confirmation-mismatch");
        tx.put("actions", { ...pending, status: "executing" });
        const executing: ToolCall = { ...storedCall, status: "executing" };
        tx.put("calls", executing);
        return executing;
      });
      try {
        await execute(context, call, signal, action);
      } catch (error) {
        if (!(error instanceof AgentError) || error.code !== "outcome-unknown") throw error;
      }
      return getAction(actionId);
    },

    async cancel(context: C, actionId: string): Promise<PendingAction> {
      const action = getAction(actionId);
      await identityFor(context, action.identity);
      return persist((tx) => {
        const current = tx.get("actions", actionId);
        if (!current) throw new AgentError("not-found");
        if (current.status === "cancelled") return current;
        if (current.status !== "pending") actionUnavailable(current);
        const cancelled: PendingAction = { ...current, status: "cancelled" };
        tx.put("actions", cancelled);
        const call = tx.get("calls", current.callId);
        if (call) tx.put("calls", { ...call, status: "rejected", error: "action-cancelled" });
        persistActionOutcome(tx, cancelled, secrets(), limits.maxOutputBytes);
        return cancelled;
      });
    },
  };
}
