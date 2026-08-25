const sessions = new Map();
let nextId = 1;

export function describe() {
  return JSON.stringify({
    abi: "lenso.json-interactions@1",
    capabilities: [{
      capability_id: "lenso.agent@1",
      descriptor_version: "1.1.0",
      request_operations: [],
      stream_operations: ["run_turn"]
    }]
  });
}

export function invoke() {
  return JSON.stringify({ ok: null });
}

export function streamOpen(capability, operation, requestJson) {
  if (capability !== "lenso.agent@1" || operation !== "run_turn") {
    throw new Error("unsupported Capability or Operation");
  }
  const id = nextId++;
  sessions.set(id, { request: JSON.parse(requestJson), emitted: false });
  return JSON.stringify({ ok: id });
}

export function streamSend() {
  return JSON.stringify({ ok: null });
}

export function streamReceive(id) {
  const session = sessions.get(id);
  if (!session) throw new Error("unknown stream");
  if (!session.emitted) {
    session.emitted = true;
    return JSON.stringify({
      ok: {
        kind: "message",
        value: {
          sequence: "0",
          text: `QuickJS plugin: ${session.request.input}`
        }
      }
    });
  }
  sessions.delete(id);
  return JSON.stringify({ ok: { kind: "terminal-success" } });
}

export function streamCloseSend() {
  return JSON.stringify({ ok: null });
}

export function streamCancel(id) {
  sessions.delete(id);
  return JSON.stringify({ ok: null });
}
