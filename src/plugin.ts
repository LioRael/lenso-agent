import { definePlugin, type Plugin, type PluginContext } from "@lenso/core";
import { createAgentService } from "./runtime";
import type { PiExtensions } from "./pi";
import type { AgentOptions, AgentService } from "./types";

export interface AgentPluginOptions<C> {
  id: string;
  requires: readonly Plugin<unknown>[];
  options(context: PluginContext): AgentOptions<C> | Promise<AgentOptions<C>>;
  /** Resolve provider/context hooks from declared dependencies during setup. */
  pi?: PiExtensions | ((context: PluginContext) => PiExtensions | Promise<PiExtensions>);
}

export function createAgentPlugin<C>(options: AgentPluginOptions<C>): Plugin<AgentService<C>> {
  return definePlugin({
    id: options.id,
    requires: options.requires,
    async setup(context) {
      const config = await options.options(context);
      const pi = typeof options.pi === "function" ? await options.pi(context) : options.pi;
      const agent = createAgentService(config, pi);
      context.onCleanup(() => agent.close());
      return agent;
    },
  });
}
