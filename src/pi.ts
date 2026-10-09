import type { AgentOptions as CoreOptions, ThinkingLevel } from "@earendil-works/pi-agent-core";
import type { RunStatus } from "./types";

export interface PiLifecycleHook {
  beforeRun?: (fact: { sessionId: string; runId: string }, signal: AbortSignal) => Promise<void>;
  afterRun?: (fact: { sessionId: string; runId: string; status: RunStatus }, signal: AbortSignal) => Promise<void>;
}

/** Provider and transcript customization only. Executable tools remain gateway-owned. */
export type PiExtensions = Partial<Pick<CoreOptions,
  "streamFn" | "getApiKey" | "transformContext" | "convertToLlm"
>> & { thinkingLevel?: ThinkingLevel; hooks?: readonly PiLifecycleHook[] };

export { createAgentService } from "./runtime";
