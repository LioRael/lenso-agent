# Codex Responses transport

## Ownership and configuration

`lenso.agent.model.openai-codex-direct` owns both SSE and WebSocket transports.
Model Descriptor 4.1 adds one optional task-affinity hint to the existing
`lenso.agent.model@4` interface. Auth dependency, Session ownership, Host routing,
and Kernel remain unchanged. There is no
separate WS Plugin, package, provider selection, or execution-adapter fallback.
Deleting the direct Model Plugin removes both transports and their resources.

The linked native Rust implementation uses its existing source-first Plugin
factory and configuration Schema. There is no new runtime implementation or
cross-runtime support claim. App owners can configure the selected Instance at
`plugins/lenso.agent.model.openai-codex-direct/model.toml` in the Agent Home:

```toml
transport = "websocket"
```

- `websocket` is the default when `transport` is omitted. It requires a
  successful WS handshake and never silently uses HTTP.
- `sse` explicitly selects HTTP streaming, including in proxy-only environments.
- `auto` attempts WS and falls back to SSE only on HTTP 426 during the handshake,
  before sending a model request. Authentication, rate-limit, timeout, and
  ambiguous network failures are not reasons to switch transports.

The existing base-URL validation still permits only the official ChatGPT backend
or a loopback HTTP fixture. The WS URL is derived from that validated URL, not
separately configurable. Redirects are not followed by the WS client. The
transport uses direct TCP/TLS; unlike the HTTP client, it does not consume
environment HTTP proxy configuration. Use SSE in proxy-only environments.

## Logical stream and connection lifetime

Both transports share Responses request conversion, Tool-name mapping, and
JSON event interpretation. Only framing and connection management differ.
WS sends `response.create`, without HTTP-only `stream` or `background` fields.
The subscription handshake uses `responses_websockets=2026-02-06`, also present
in the locally inspected official Codex 0.153.2 binary.

Each completion supplies full input. A matching, explicitly scoped checkpoint
may replace its wire input with incremental Tool results or user messages; a
socket alone never implies conversation identity. A logical stream's
`close_send` does not close the physical connection. Only successful explicit
completion returns a socket to the pool; cancellation, malformed events,
truncation, timeout, and dropped unfinished streams discard it. No partially
observed model step is replayed.

The pool belongs to one fresh Plugin Instance generation:

- at most four leased/idle connections combined, one active response per socket;
- no unbounded wait queue; saturation returns the existing overload error;
- account and access-token changes invalidate idle connections;
- idle connections expire after 60 seconds, pruned at checkout and every 30 seconds;
- connections older than 50 minutes are not reused;
- connect/send timeout is 20 seconds, response-event idle timeout is 300 seconds;
- incoming frames/messages obey `max_event_bytes`, outbound requests are capped
  at 8 MiB, and idle control-frame draining is bounded;
- generation-managed maintenance and deactivation discard idle resources;
  active leased work is drained by the existing Host lifecycle;
- diagnostics contain no handshake headers, credentials, or provider bodies.

The existing Agent Loop remains the only model-open retry owner. A failed WS
send is non-retryable because upstream acceptance is unknown. HTTP transport
errors are likewise retryable only when connection establishment failed.

## Verification

```sh
cargo test -p lenso-agent-model-openai-codex-direct-plugin
cargo test -p lenso-agent-cli --test openai_codex_direct
cargo clippy -p lenso-agent-model-openai-codex-direct-plugin --all-targets --locked -- -D warnings
```

Local tests cover reusable independent responses, credential rotation,
admission bounds, cancellation during receive, oversized frames, partial output
followed by disconnect, explicit upgrade rejection, and sanitized errors.
The CLI fixture exercises authenticated catalog discovery and a complete
read-only Tool round trip on one WS connection through the real Host and Agent.
Another CLI fixture exercises the `auto` HTTP fallback.

An ignored `live_subscription_websocket_smoke` test is opt-in through
`LENSO_CODEX_WS_SMOKE_CREDENTIAL`. It uses the Auth Plugin, an isolated temporary
Agent Home, and the official subscription endpoint. It asks the model to read
an isolated marker file and return its contents. It sends real model requests
and must not run in ordinary CI or read developer credentials implicitly.

## Incremental continuation

The optional `continuation_scope` in Model 4.1 identifies an isolated task. The
Agent Loop supplies its random Turn ID; auxiliary model callers omit the hint.
The Plugin prefers an idle socket with that scope, then validates exact prior
input and all wire controls. It matches projected assistant text and ordered
Tool call/result pairs before sending only new results/user input plus the last
successful response ID. A changed prefix, model, instructions, tools, or controls
causes a full request. Checkpoints are capped at 8 MiB and are never persisted.

Only an explicit `previous_response_not_found` as the first event permits one
full-input retry; disconnects or any preceding event disable this recovery.
No raw reasoning is retained in the checkpoint. Server-side response state is
not durable Session storage. Cross-Turn continuation, public API multiplexing,
and steering are not assumed. See [ADR-0097](../adr/0097-keep-model-continuation-an-optional-affinity-hint.md).

Protocol reference: [OpenAI Responses WebSocket mode](https://developers.openai.com/api/docs/guides/websocket-mode).
