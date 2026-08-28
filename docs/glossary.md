# Architecture glossary

| Term | Meaning | Public? |
| --- | --- | --- |
| Host Catalog | Immutable description of linked Plugins, default Instances, root Slots, and Host-private attachments generated for one Host build. | Reference only |
| Plugin Root | The App owner's `plugins/` directory. One directory per Plugin contains an optional package, Instance TOML files, and optional same-name resource directories. | Yes |
| Plugin | The only removable behavior and distribution unit. A built-in and an external Plugin have the same configuration model. | Yes |
| Plugin Instance | One configured occurrence of a Plugin, represented by `<instance>.toml`; `default.toml` is the common case. | Yes |
| Resolved App Plan | Complete immutable execution input derived from one Host Catalog and one Plugin Root snapshot. It is replay evidence, not source configuration. | Diagnostic |
| App Generation | A ready Plan plus its runtime resources. Existing Turns retain their leased Generation while new work switches atomically. | Operational |
| Controller | Internal state machine that stages, readies, switches, drains, and recovers Generations. | No |
| Supervisor | Runtime owner that starts and stops Plugin Instances according to one Plan. | No |
| Receipt | Internal evidence that a package, candidate, or transition was checked under exact authority. | No |
| Store | Internal content-addressed artifact storage, when durable external Plugin bytes require it. It is not App configuration. | No |

The dependency direction is `Host Catalog + Plugin Root snapshot -> Resolved
App Plan -> App Generation -> Kernel`. Users never author a Plan, binding graph,
Active Set, Receipt, Store entry, Controller state, or Supervisor instruction.
