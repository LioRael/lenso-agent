import { createModels } from "@earendil-works/pi-ai";
import { openaiProvider } from "@earendil-works/pi-ai/providers/openai";
import type { PiExtensions } from "../src/pi";

/** Configuration sketch only. The host supplies its credential callback, not chat/context. */
export function providerConfiguration(modelId: string, getApiKey: (provider: string) => Promise<string | undefined>) {
  const models = createModels();
  models.setProvider(openaiProvider());
  const model = models.getModels("openai").find(model => model.id === modelId);
  if (!model) throw new Error("provider-model-unavailable");
  const pi: PiExtensions = {
    streamFn: models.streamSimple.bind(models),
    getApiKey,
  };
  return { model, pi };
}

// Host wiring: providerConfiguration("<published-openai-model-id>", async () => "<API_KEY_FROM_HOST>");
