# @lenso/contract-runtime

Runtime-neutral TypeScript support shared by bindings generated with
`lenso-contract-codegen`.

The package owns portable wire primitives, Invocation and failure types,
forward-compatible Domain Error decoding, Stream/Event support types, and
portable JSON number validation. It does not own Capability-specific values,
Clients, Providers, operation dispatch, networking, retries, authentication,
or product policy.

Patch and minor releases preserve wire behavior. Changes to that behavior
require an explicit compatibility decision plus coordinated Rust, TypeScript,
conformance, and generated-artifact updates.

The package publishes ESM, CommonJS, and TypeScript declarations and contains
no platform or runtime dependencies.

```sh
npm install @lenso/contract-runtime
```

Generated bindings import this package directly. Applications normally use
the re-exported types and codecs from their generated binding rather than
calling the runtime helpers themselves.

Executable browser clients generated from a Descriptor import the
`@lenso/contract-runtime/browser` subpath for portable JSON Schema and result
envelope validation.
