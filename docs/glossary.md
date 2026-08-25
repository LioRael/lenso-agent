# Control-plane glossary

| Term | Owner and lifecycle | Stored form |
| --- | --- | --- |
| App Definition | App author selects the base Module instances, configuration, and bindings before resolution. | root `lenso.app.json` |
| Resolved App Plan | The resolver creates the immutable execution graph consumed by the Kernel. It never changes in place. | generated `.lenso/resolved-plan.json` |
| Plugin Manifest | Plugin publisher declares one exact release, artifacts, capabilities, permissions, and evidence. | `lenso-plugin.json` inside a Bundle |
| Plugin lock | Host records the admitted Plugin releases selected for the next resolution. | Content-addressed control-plane state |
| App Generation | Host stages one resolved Plan plus its exact runtime resources, then marks it ready before switching traffic. | `.lenso/plugins/generations/<digest>` |
| Active Set | Host-owned pointer to the active and rollback-eligible Generations. | Atomic generation-control state |
| Authority | Host-owned right to perform one transition or serve one active Generation. It is fenced across processes. | Generation authority records and leases |
| Receipt | Durable evidence that a transition, readiness gate, or compatibility check completed under a specific authority. | Generation-control records |
| Profile Catalog | Product-owned policy that maps reviewed Plugin identities to executable factories, risk, and evidence requirements. | Rust source in the host product |
| Catalog | Legacy distribution/control-plane term. It is not the vNext package-manager path and should not be used for new App composition. | Legacy-only services and documents |

The dependency direction is: an App Definition resolves to a Plan; admitted
Plugin Manifests update a Plugin lock; the host stages a new Generation from
those immutable inputs; authority and receipts govern the switch; the Active
Set records which Generation may receive new work.
