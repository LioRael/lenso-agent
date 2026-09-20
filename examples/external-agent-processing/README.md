# External Agent Processing Fixture

This source-local fixture proves that a third party can replace the default
Agent Loop while using the public `lenso.agent.turn-processing@1` Capability
for narrow, ordered extensions.

It links five independently authored Plugins:

- `example.alternate-agent-loop` provides `lenso.agent@3` without depending on
  `lenso-agent-loop-plugin`;
- `example.inspect-model` provides `lenso.agent.model@4` and returns the exact
  model-visible input it receives;
- two configured instances of `example.turn-processor` project model requests,
  transform Tool arguments, and redact Tool-result presentation in Plan order;
- `example.secret-tool` requires its final JSON Schema argument to be
  `{"id":"approved"}`; and
- `example.final-approval` allows only that final argument, so approval cannot
  be reused from the model's original `{"id":"raw"}` request.

The alternate Loop invokes the aggregate Tools runtime, records the immutable
Tool fact, projects only its Model presentation, and then calls the Model. The
test asserts that the Model sees both request processors in order and the
redacted result, never the unredacted factual Tool output.

Run the verifier from an Agent checkout. It copies this fixture to a temporary
directory and only patches that copy to local framework sources:

```sh
python3 examples/external-agent-processing/verify-foundation.py \
  --framework-root /path/to/framework \
  --agent-root /path/to/lenso-agent
```

This is V04 source/local evidence, not released-artifact qualification. The
fixture deliberately relies on published dependency names and the verifier's
temporary source patches; V10 requires a separately published, clean install.
