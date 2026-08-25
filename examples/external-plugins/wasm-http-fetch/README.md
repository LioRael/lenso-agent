# External Wasm HTTP fetch Plugin

This standalone example provides one Tool from a Wasm component. The Tool can issue a bounded HTTP GET only through the exact `lenso.agent.http-fetch@1` Host binding selected by the Harness profile. Its required `network` Permission Request is approved into the Plugin Set and enforced again by the App-selected HTTP Provider origin allowlist.

Installation requires explicit review evidence. The example intentionally has no path dependency on the Harness checkout; it pins the generated Capability package and guest SDK by Git revision.

The Manifest template uses a placeholder loopback origin. A real Bundle must replace it with a canonical `http://` or `https://` origin, with no credentials, path, query, or fragment. Review approves only that exact set. The App's `lenso.agent.http-fetch` configuration must independently allow the same origins; the checked-in `headless-network` Composition starts with an empty allowlist.

The Provider disables redirects and bounds timeout and response size. It returns UTF-8 response bodies only. The Wasm component receives no raw socket or generic WASI network authority.
