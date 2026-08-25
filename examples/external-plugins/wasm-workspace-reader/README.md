# External Wasm workspace reader Plugin

This standalone example provides one Tool from a Wasm component. The Tool can read workspace text only through the exact `lenso.agent.tool-provider@1` binding selected by the Harness Host Profile. The Plugin does not receive ambient filesystem, network, process, Secrets, state, or data-mount authority.

Installation requires explicit review evidence:

```sh
lenso-agent-cli plugins install --bundle ./bundle --evidence review-ticket-123
```

The example intentionally contains no path dependency on the Harness repository. It pins the generated Workspace Read Capability package and the guest SDK by Git revision.
