const sessions = new Map();
let nextId = 1;

const MODEL = "lenso.agent.model@1";
const PROMPT = "lenso.agent.prompt@1";
const SESSION = "lenso.agent.session@1";
const TOOLS = "lenso.agent.tools@2";

function unwrap(encoded, label) {
  const envelope = JSON.parse(encoded);
  if (envelope.runtime) throw new Error(`${label} Runtime Failure: ${envelope.runtime.kind}`);
  if (envelope.error !== undefined) throw new Error(`${label} Domain Error`);
  return envelope.ok;
}

function bindingsByCapability() {
  const bindings = unwrap(lensoHostBindings(), "bindings");
  return new Map(bindings.map((binding) => [binding.capability_id, binding]));
}

function invokeHost(binding, operation, request) {
  return unwrap(
    lensoHostInvoke(binding.binding_id, operation, JSON.stringify(request)),
    `${binding.capability_id}.${operation}`
  );
}

export function describe() {
  return JSON.stringify({
    abi: "lenso.json-host-imports@1",
    capabilities: [{
      capability_id: "lenso.agent@1",
      descriptor_version: "1.2.0",
      request_operations: [],
      stream_operations: ["run_turn"]
    }],
    required_capabilities: [
      { capability_id: MODEL, descriptor_version: "1.1.0", cardinality: "one" },
      { capability_id: PROMPT, descriptor_version: "1.0.0", cardinality: "one" },
      { capability_id: SESSION, descriptor_version: "1.1.0", cardinality: "one" },
      { capability_id: TOOLS, descriptor_version: "2.0.0", cardinality: "one" }
    ]
  });
}

export function invoke() {
  return JSON.stringify({ ok: null });
}

export function streamOpen(capability, operation, requestJson) {
  if (capability !== "lenso.agent@1" || operation !== "run_turn") {
    throw new Error("unsupported Capability or Operation");
  }
  const request = JSON.parse(requestJson);
  const bindings = bindingsByCapability();
  const openedSession = invokeHost(
    bindings.get(SESSION),
    "open",
    request.session_id ? { session_id: request.session_id } : {}
  );
  const prompt = invokeHost(bindings.get(PROMPT), "assemble", {});
  const tools = invokeHost(bindings.get(TOOLS), "catalog", {});
  const modelStream = unwrap(
    lensoHostStreamOpen(
      bindings.get(MODEL).binding_id,
      "complete",
      JSON.stringify({
        model: "fixture/readme-summary-v1",
        messages: [
          { role: "system", content: prompt.content },
          { role: "user", content: `Answer directly: ${request.input}` }
        ],
        tools: tools.tools,
        temperature: 0,
        max_output_tokens: 128
      })
    ),
    "model.complete"
  );
  const id = nextId++;
  sessions.set(id, {
    modelStream,
    sessionId: openedSession.session_id,
    sequence: 0
  });
  return JSON.stringify({ ok: id });
}

export function streamSend() {
  return JSON.stringify({ ok: null });
}

export function streamReceive(id) {
  const session = sessions.get(id);
  if (!session) throw new Error("unknown stream");
  while (true) {
    const frame = unwrap(lensoHostStreamReceive(session.modelStream), "model.receive");
    if (frame.kind === "message") {
      if (frame.value.kind !== "text_delta" || frame.value.text.length === 0) continue;
      return JSON.stringify({
        ok: {
          kind: "message",
          value: {
            kind: "text_delta",
            sequence: String(session.sequence++),
            text: frame.value.text,
            session_id: session.sessionId
          }
        }
      });
    }
    if (frame.kind === "peer-half-closed") continue;
    sessions.delete(id);
    if (frame.kind === "terminal-error") {
      return JSON.stringify({ ok: { kind: "terminal-error", value: "context_limit_exceeded" } });
    }
    return JSON.stringify({ ok: { kind: "terminal-success" } });
  }
}

export function streamCloseSend() {
  return JSON.stringify({ ok: null });
}

export function streamCancel(id) {
  const session = sessions.get(id);
  if (session) lensoHostStreamCancel(session.modelStream);
  sessions.delete(id);
  return JSON.stringify({ ok: null });
}
