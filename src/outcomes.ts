import { AgentError, jsonCopy, safeValue, sameIdentity } from "./safety";
import type { ActionStatus, Message, PendingAction, StoreTransaction } from "./types";

type TerminalActionStatus = Exclude<ActionStatus, "pending" | "executing">;
const metadataBytes = 32_768;
const label = (value: string): string => value.length <= 512 ? value : "[omitted: oversized label]";

export function isTerminalAction(action: PendingAction): action is PendingAction & { status: TerminalActionStatus } {
  return action.status === "completed" || action.status === "rejected" ||
    action.status === "outcome_unknown" || action.status === "cancelled" || action.status === "expired";
}

export function isActionOutcomeMessage(message: Message): boolean {
  return message.source?.kind === "action-outcome" &&
    message.id === `action-outcome:${message.source.actionId}` && message.value.role === "user";
}

export function projectActionOutcome(
  tx: StoreTransaction,
  action: PendingAction,
  secrets: readonly string[],
  maxOutputBytes: number,
): Message | undefined {
  if (!isTerminalAction(action)) return undefined;
  const call = tx.get("calls", action.callId);
  if (!call || call.actionId !== action.id || call.sessionId !== action.sessionId ||
      call.runId !== action.runId || !sameIdentity(call.identity, action.identity))
    throw new AgentError("execution-state-conflict");
  const metadata = jsonCopy(safeValue({
    source: "action-outcome",
    actionId: action.id,
    callId: action.callId,
    runId: action.runId,
    modelCallId: label(call.modelCallId),
    toolName: label(action.prepared.definition.name),
    status: action.status,
    error: action.error ? label(action.error) : (action.status === "cancelled" ? "action-cancelled" :
      action.status === "expired" ? "action-expired" : null),
  }, secrets), metadataBytes);
  const fact = jsonCopy({
    ...metadata,
    ...(action.status === "completed" && action.result !== undefined
      ? { result: jsonCopy(safeValue(jsonCopy(action.result, maxOutputBytes), secrets), maxOutputBytes) } : {}),
  }, metadataBytes + maxOutputBytes);
  const now = Date.now();
  return {
    id: `action-outcome:${action.id}`,
    sessionId: action.sessionId,
    runId: action.runId,
    source: { kind: "action-outcome", actionId: action.id, callId: action.callId },
    createdAt: now,
    value: { role: "user", content: JSON.stringify(fact), timestamp: now },
  };
}

/** The caller writes the terminal action in this same transaction. Never invoke a tool here. */
export function persistActionOutcome(
  tx: StoreTransaction,
  action: PendingAction,
  secrets: readonly string[],
  maxOutputBytes: number,
): void {
  if (!isTerminalAction(action) || tx.get("messages", `action-outcome:${action.id}`)) return;
  const message = projectActionOutcome(tx, action, secrets, maxOutputBytes);
  if (message) tx.put("messages", message);
}

/** Backfill is bounded before materializing legacy rows; authorization belongs to the caller. */
export function ensureActionOutcomes(
  tx: StoreTransaction,
  sessionId: string,
  secrets: readonly string[],
  maxOutputBytes: number,
  maxSnapshotBytes: number,
  allowAction: (action: PendingAction) => boolean,
): void {
  if (tx.bytes("actions", sessionId) > maxSnapshotBytes) throw new AgentError("snapshot-budget");
  for (const action of tx.list("actions", sessionId)) {
    if (isTerminalAction(action) && allowAction(action))
      persistActionOutcome(tx, action, secrets, maxOutputBytes);
  }
}
