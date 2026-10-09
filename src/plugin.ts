import { definePlugin, type Plugin, type PluginContext } from "@lenso/core";
import { createAgentService } from "./runtime";
import type { PiExtensions } from "./pi";
import type { AgentOptions, AgentService } from "./types";

export interface AgentPluginOptions<C> {
  id: string;
  requires: readonly Plugin<unknown>[];
  options(context: PluginContext): AgentOptions<C> | Promise<AgentOptions<C>>;
  pi?: PiExtensions;
}

export function createAgentPlugin<C>(options: AgentPluginOptions<C>): Plugin<AgentService<C>> {
  return definePlugin({
    id: options.id,
    requires: options.requires,
    async setup(context) {
      const agent = createAgentService(await options.options(context), options.pi);
      context.onCleanup(() => agent.close());
      return agent;
    },
  });
}
