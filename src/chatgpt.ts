import { streamSimple } from "@earendil-works/pi-ai/api/openai-responses";
import type { Model } from "@earendil-works/pi-ai";
import type { PiExtensions } from "./pi";

const unsupported = ["background", "conversation", "max_output_tokens", "max_tool_calls", "metadata", "moderation", "multi_agent", "prompt", "prompt_cache_retention", "prompt_cache_options", "safety_identifier", "temperature", "top_logprobs", "top_p", "truncation", "user", "previous_response_id"];
/** Official SIWC preview uses the public Responses endpoint, not the legacy Codex backend. */
export function chatGPTPayload(value: unknown, toolNamespace = "agent") {
  if (!/^[A-Za-z_][A-Za-z0-9_]{0,63}$/.test(toolNamespace)) throw new Error("Invalid ChatGPT tool namespace");
  const payload = structuredClone(value) as Record<string, any>;
  if (!payload || typeof payload !== "object" || !Array.isArray(payload.input)) throw new Error("Invalid Responses request");
  for (const key of unsupported) delete payload[key];
  payload.store = false; payload.stream = true;
  payload.input = payload.input.map((item: any) => item.role === "system" ? { ...item, role: "developer" } : item);
  if (payload.tools?.length) {
    if (!payload.tools.every((tool: any) => ["function", "custom"].includes(tool.type))) throw new Error("Unsupported ChatGPT plan tool");
    payload.tools = [{ type: "namespace", name: toolNamespace, description: "Host-selected tools", tools: payload.tools }];
  }
  return payload;
}
/** The host has already verified SIWC consent, scopes, identity and expiry. */
export type ChatGPTAccess = { accessToken: string; registrationId: string; modelId?: string | null };

/** Trusted server-side adapter. Never accept owner/selection directly from model input. */
export interface ChatGPTAuthAdapter {
  getAccess(owner: string, input: { registrationId: string }): Promise<ChatGPTAccess>;
}
export type ChatGPTProviderOptions = ChatGPTAuthAdapter & {
  owner: string;
  registrationId: string;
  modelId: string;
  toolNamespace?: string;
  fetch?: typeof globalThis.fetch;
};
export type ChatGPTProvider = {
  model: Model<"openai-responses">;
  /** Transient values for Agent redaction; do not serialize or expose to clients. */
  secrets: () => readonly string[];
  pi: PiExtensions & {
    getApiKey: NonNullable<PiExtensions["getApiKey"]>;
    streamFn: NonNullable<PiExtensions["streamFn"]>;
  };
};

/** No I/O at construction. Host owns OAuth, private storage and executor invalidation. */
export function createChatGPTProvider(inputOptions: ChatGPTProviderOptions): ChatGPTProvider {
  // Snapshot the selection so mutating a host options object cannot retarget this provider.
  const options = { ...inputOptions };
  for (const value of [options.owner, options.registrationId, options.modelId]) {
    if (typeof value !== "string" || !value.trim()) throw new Error("Explicit ChatGPT owner, registration and model required");
  }
  const toolNamespace = options.toolNamespace ?? "agent";
  chatGPTPayload({ input: [] }, toolNamespace);
  const secrets = new Set<string>();
  const model: Model<"openai-responses"> = { id: options.modelId, name: options.modelId, api: "openai-responses", provider: "openai", baseUrl: "https://api.openai.com/v1",
    input: ["text"], reasoning: false, contextWindow: 32768, maxTokens: 4096,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, compat: { supportsMaxOutputTokens: false } };
  return { model, secrets: () => [...secrets], pi: {
    async getApiKey(provider) {
      if (provider !== "openai") throw new Error("Provider mismatch");
      const access = await options.getAccess(options.owner, { registrationId: options.registrationId });
      if (!access.accessToken || access.registrationId !== options.registrationId || access.modelId !== options.modelId) throw new Error("Connection changed; select the account and model again");
      secrets.add(access.accessToken);
      return access.accessToken;
    },
    streamFn(current, context, input) {
      if (!input?.apiKey || current.id !== options.modelId || current.baseUrl !== model.baseUrl) throw new Error("Explicit selected ChatGPT credential required");
      const guardedFetch = (async (url: any, init?: RequestInit) => {
        const destination = new URL(url instanceof Request ? url.url : String(url));
        if (destination.origin !== "https://api.openai.com" || destination.pathname !== "/v1/responses" || destination.search || destination.username || destination.password || destination.hash) throw new Error("Unexpected ChatGPT transport destination");
        const headers = new Headers(init?.headers ?? (url instanceof Request ? url.headers : undefined));
        if (headers.get("authorization") !== `Bearer ${input.apiKey}`) throw new Error("Unexpected ChatGPT credential");
        headers.delete("openai-organization"); headers.delete("openai-project");
        return (options.fetch ?? globalThis.fetch)(url, { ...init, headers, redirect: "error" });
      }) as typeof globalThis.fetch;
      return streamSimple(model, context, { ...input, fetch: guardedFetch, headers: { "OpenAI-Organization": null, "OpenAI-Project": null }, maxRetries: 0, transport: "sse", cacheRetention: "none", timeoutMs: 90000,
        onPayload: value => chatGPTPayload(value, toolNamespace),
        onProviderStreamEvent(event: any) {
          if (event?.type === "response.incomplete") throw new Error("ChatGPT response was incomplete");
        },
      });
    },
  } };
}
