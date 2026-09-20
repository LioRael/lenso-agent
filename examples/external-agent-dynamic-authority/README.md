# External Agent Dynamic Authority Fixture

This copied, source-local fixture exercises the optional dynamic authorization
path around the public Agent Tool runtime. It uses four independent Plugins:

- a caller that consumes its selected Tool and authority contracts;
- a tenant policy Provider that derives selection identity from the resolved
  caller and reads revocation state;
- the optional `lenso.agent.tools` aggregate runtime; and
- a Tool Provider that records an observable side effect only after final
  authorization.

The host asks the selected policy Provider for a precise `ResourceIdentity`,
then seals the returned snapshot into an invocation context. The Tool runtime
revalidates that sealed snapshot after argument normalization and schema
validation, before the Tool Provider runs. An ordinary extension with the same
key is rejected as a forgery. Calls without a selected grant preserve the
existing static Plan path.

Run it from an Agent checkout:

```sh
python3 examples/external-agent-dynamic-authority/verify-foundation.py \
  --framework-root /path/to/framework \
  --agent-root /path/to/lenso-agent
```

This is V08 source/local evidence. It does not claim a published package or a
cross-target distribution qualification.
