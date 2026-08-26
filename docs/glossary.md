# Control-plane glossary

| Term | Owner and lifecycle | Stored form |
| --- | --- | --- |
| App Definition | App author selects the base Module instances, configuration, and bindings before resolution. | root `lenso.app.json` |
| Resolved App Plan | The resolver creates the immutable execution graph consumed by the Kernel. It never changes in place. | in-memory value owned by one App Generation; explicit files are accepted only for advanced replay |
| Plugin Manifest | Plugin publisher declares one exact release, artifacts, capabilities, permissions, and evidence. | `lenso-plugin.json` inside a Bundle |
| Enabled Plugin intent | App owner selects versioned Host-built Plugin Profiles without copying runtime authority into source. | `extensions.lenso.agent.plugins.enabled` in `lenso.app.json` |
| Plugin lock | Host derives exact selected Plugin releases for one candidate resolution. | in-memory authority on the source-backed bundled path |
| App Generation | Host stages one resolved Plan plus its exact runtime resources, then marks it ready before switching traffic. | in-memory runtime state on the source-backed bundled path |
| Active Set | Legacy Store workflow pointer to active and rollback-eligible Generations. | absent from the source-backed bundled path |
| Authority | Host-owned right to perform one transition or serve one active Generation. It is fenced across processes. | Generation authority records and leases |
| Receipt | Durable evidence that a transition, readiness gate, or compatibility check completed under a specific authority. | Generation-control records |
| Profile Catalog | Product-owned policy that maps reviewed Plugin identities to executable factories, risk, and evidence requirements. | Rust source in the host product |
| Catalog | Legacy distribution/control-plane term. It is not the vNext package-manager path and should not be used for new App composition. | Legacy-only services and documents |

The dependency direction is: an App Definition resolves to a Plan; admitted
Plugin Manifests update a Plugin lock; the host stages a new Generation from
those immutable inputs; authority and receipts govern the switch; the Active
Set records which Generation may receive new work.
