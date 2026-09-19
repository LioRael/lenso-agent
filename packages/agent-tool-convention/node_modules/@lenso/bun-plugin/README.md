# @lenso/bun-plugin

`definePlugin(...)` describes one Plugin. Generated Capability values keep the author API typed while the SDK owns process startup, construction, dependency injection, cancellation, shutdown, limits, and Runtime Failure mapping.

```ts
import { definePlugin } from "@lenso/bun-plugin";
import { Conversation } from "./generated/conversation.ts";
import { Store } from "./generated/store.ts";

export default definePlugin({
  provides: [Conversation],
  dependencies: { store: Store.required() },

  async create({ dependencies }) {
    return {
      async *chat(_context, request) {
        const greeting = await dependencies.store.get(request.room);
        yield { text: greeting ?? "Hello" };
      },
    };
  },
});
```

`provides` accepts generated Capability values. TypeScript checks that the object returned by `create` implements every generated Provider interface in that list. The runtime binds those methods to the admitted endpoints; authors do not implement `CapabilityProviderBinding`, `openStream`, `publishEvent`, the process handshake, or the transport.

A Request-only generated Contract also supplies `required()`, `optional()`, and `many()`. The dependency table key is the default stable requirement id, so `store: Store.required()` needs no repeated string. Pass an explicit id only when it must differ from the local name. The Host injects only the exact provider routes selected by the immutable Plan.

`create` runs once for each admitted Plugin instance and receives decoded configuration plus generated dependency clients. It returns the instance whose methods provide behavior. `stop` runs at most once during managed shutdown. A Plugin with no provided Capability is valid when it only consumes dependencies or owns lifecycle work.

For a server-output Stream, a Provider may return an `AsyncIterable` or async generator. The SDK preserves pull-based backpressure, maps consumer cancellation to `iterator.return()`, accepts the consumer half-close, and reports an unexpected inbound message as a protocol violation. A Provider that needs bidirectional messages, independent half-close, terminal domain errors, or custom cancellation returns the generated `StreamSession` interface instead.

Event handlers may return `void` or `Promise<void>`. Publication waits for completion; a rejected Promise becomes a Plugin Runtime Failure rather than an unhandled background rejection.

Operation names such as `chat`, `notify`, or `get` come only from the Capability Descriptor. They are examples, not Plugin hooks or reserved SDK methods. Products such as Agent may lower their own `tools` syntax into an ordinary Capability, while the generic Plugin API remains product-neutral.

This release supports Request, Stream, and Event providers and generated
outbound dependency clients over the Bun Authoring V2 process runtime. The
same `required()`, `optional()`, and `many()` declarations work for every
interaction kind. Stream clients preserve ordered messages, half-close,
terminal domain outcomes, and cancellation. Event clients report the bounded
admission result for every exact Plan-selected subscriber.

Rust Process and Rust Wasm guests expose the same three Capability interaction
kinds through their generated clients. Execution mechanics still differ by
Adapter; the portable contract, requirement identity, and Host-selected routes
do not.

Generated entrypoints call the low-level Bun serving functions. Authors export the definition and do not call `serve` themselves. The older `providers`, `provider(...)`, `bind*Provider(...)`, and raw binding types remain as a compatibility and Adapter-lowering seam, but ordinary authoring should use generated Capability values through `provides`.
