import type { AgentMessage } from "@earendil-works/pi-agent-core";
import type { Model, Api } from "@earendil-works/pi-ai";

/** Derived by the host from authenticated evidence, never from business JSON. */
export interface Identity {
  subject: string;
  scope: string;
  application: string;
  target: string;
}
export type RunStatus = "running" | "completed" | "failed" | "cancelled" | "interrupted";
export type CallStatus = "prepared" | "awaiting_confirmation" | "executing" | "completed" | "rejected" | "outcome_unknown";
export type ActionStatus = "pending" | "executing" | "completed" | "cancelled" | "expired" | "rejected" | "outcome_unknown";
export interface Session {
  id: string;
  identity: Identity;
  profile: string;
  createdAt: number;
}
export interface Message {
  id: string;
  sessionId: string;
  runId: string;
  createdAt: number;
  value: AgentMessage;
}
export interface Usage {
  model: string;
  provider: string;
  inputTokens: number | null;
  outputTokens: number | null;
  toolCalls: number;
  modelTurns: number;
  durationMs: number;
}
export interface Run {
  id: string;
  sessionId: string;
  status: RunStatus;
  createdAt: number;
  finishedAt?: number;
  error?: string;
  usage?: Usage;
}
export interface ToolDefinition {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
  target: { application: string; instance: string; plugin: string; operation: string };
  effect: "read" | "write" | "unknown";
  confirmation: boolean;
}
export interface PreparedTool {
  definition: ToolDefinition;
  input: unknown;
  precondition: string | null;
}
export interface ToolCall {
  id: string;
  sessionId: string;
  runId: string;
  modelCallId: string;
  identity: Identity;
  prepared: PreparedTool;
  status: CallStatus;
  createdAt: number;
  result?: unknown;
  error?: string;
  actionId?: string;
}
export interface PendingAction {
  id: string;
  callId: string;
  sessionId: string;
  runId: string;
  identity: Identity;
  prepared: PreparedTool;
  digest: string;
  createdAt: number;
  expiresAt: number;
  status: ActionStatus;
  result?: unknown;
  error?: string;
}
export interface Tables {
  sessions: Session;
  messages: Message;
  runs: Run;
  calls: ToolCall;
  actions: PendingAction;
}
export interface StoreTransaction {
  get<K extends keyof Tables>(table: K, id: string): Tables[K] | undefined;
  list<K extends keyof Tables>(table: K, sessionId?: string): Tables[K][];
  put<K extends keyof Tables>(table: K, record: Tables[K]): void;
}
/** Work must be synchronous; implementations commit atomically or roll back. */
export interface AgentStore {
  transaction<T>(work: (tx: StoreTransaction) => T): T;
  close(): void;
}
export interface ToolSource<C> {
  catalog(context: C, identity: Identity, signal: AbortSignal): Promise<readonly ToolDefinition[]>;
  prepare(name: string, input: unknown, context: C, identity: Identity, signal: AbortSignal): Promise<PreparedTool>;
  /** Called only by the execution gateway, after authorization and durable admission. */
  invoke(prepared: PreparedTool, context: C, identity: Identity, signal: AbortSignal, confirmed: boolean): Promise<unknown>;
}
export interface ContextPart {
  source: string;
  text: string;
}
export interface AgentProfile<C> {
  id: string;
  instructions: string;
  tools: readonly string[];
  context?: (context: C, identity: Identity, signal: AbortSignal) => Promise<readonly ContextPart[]>;
}
export interface RunLimits {
  maxConcurrentRuns: number;
  maxModelTurns: number;
  maxToolCalls: number;
  maxInputBytes: number;
  maxOutputBytes: number;
  maxContextBytes: number;
  maxHistoryBytes: number;
  maxSnapshotBytes: number;
  timeoutMs: number;
  eventBuffer: number;
  actionTtlMs: number;
}
export type EventKind = "run_started" | "text_delta" | "tool_prepared" | "tool_executing" | "tool_result" | "pending_action" | "safe_error" | "run_completed" | "run_failed" | "run_cancelled" | "run_interrupted";
export interface AgentEvent {
  id: string;
  sequence: number;
  timestamp: number;
  sessionId: string;
  runId: string;
  kind: EventKind;
  callId?: string;
  actionId?: string;
  data?: unknown;
}
export interface RunHandle {
  run: Run;
  /** New live events only. Returning/detaching does NOT cancel the run. */
  events: AsyncIterable<AgentEvent>;
  done: Promise<Run>;
}
export interface AgentOptions<C> {
  store: AgentStore;
  identity: (context: C) => Promise<Identity>;
  tools: ToolSource<C>;
  profiles: readonly AgentProfile<C>[];
  model: Model<Api>;
  limits?: Partial<RunLimits>;
  secrets?: () => readonly string[];
  /** Safe audit facts only; failures fail closed before execution. No global SDK. */
  audit?: (fact: { kind: string; sessionId: string; runId: string; callId?: string; actionId?: string }) => Promise<void>;
}
export interface AgentService<C> {
  createSession(context: C, profile: string): Promise<Session>;
  readSession(context: C, sessionId: string): Promise<{ session: Session; messages: Message[]; runs: Run[]; actions: PendingAction[]; calls: ToolCall[] }>;
  startRun(context: C, sessionId: string, text: string): Promise<RunHandle>;
  getRun(context: C, runId: string): Promise<Run>;
  events(context: C, runId: string): Promise<AsyncIterable<AgentEvent>>;
  cancelRun(context: C, runId: string): Promise<Run>;
  confirmAction(context: C, actionId: string, digest: string): Promise<PendingAction>;
  cancelAction(context: C, actionId: string): Promise<PendingAction>;
  /** Explicit exclusive maintenance, only after the previous executor has stopped. */
  recoverInterrupted(): Promise<void>;
  /** Cancels and drains owned runs. Does not close borrowed storage or RunningApp. */
  close(): Promise<void>;
}
