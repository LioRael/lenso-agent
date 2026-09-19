# @lenso/process-protocol

Runtime-neutral TypeScript types, strict JSON decoding, proof framing, and
conformance helpers for `lenso-process-jsonrpc-http-v1`. The package also
exports transport-neutral Authoring V2 values and validators; Process and Bun
adapters supply their own envelopes.

Execution Adapters and child SDKs use this package at their wire boundary. It
does not spawn processes, open HTTP listeners, generate random secrets, perform
HMAC, interpret Capability payloads, or own Plugin behavior.

The published `schemas/process-protocol-v1.schema.json` describes every V1
request and successful-response envelope. Runtime validators additionally
enforce constraints JSON Schema cannot express, including duplicate-key
rejection, canonical array ordering, and expected identity/session equality.
