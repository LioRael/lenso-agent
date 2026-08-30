# ADR-0071: Project Provider and Model catalogs from the active Generation

Status: Accepted

Refined by: ADR-0081

## Context

Users need to see which Model Providers the Host build can compose, how each
Provider authenticates, which protocol features it implements, and the exact
Provider and model selected by the running App. Starting every linked Provider
to ask for metadata would activate otherwise unused Plugins. A mutable catalog
Capability or Kernel registry would also create authority outside the immutable
Host Catalog and resolved App Plan.

Authentication metadata must be useful without reading or serializing a
credential. Online Plugin reconciliation further means that the author-owned
desired files can differ from the active Generation after a rejected change or
automatic rollback.

## Decision

- The Agent Host projects a read-only Provider and Model catalog from the
  immutable linked Host Catalog plus the exact resolved Plan retained for the
  active Generation digest.
- The projection is a Host surface, not a new Capability and not a Plugin
  registry. It never activates an unselected Model Provider.
- Each Provider entry exposes its stable Provider and Plugin identities,
  configured Instances, selected Instance, model identities, typed
  authentication method, and protocol capability booleans.
- Authentication reports only `none`, a Secret Capability reference, or an
  interactive OAuth method identity. Credential values, files, tokens, and
  environment variable names are outside the schema.
- The Web surface publishes `GET /api/console/v1/agent/models`; the headless CLI
  publishes `lenso-agent-cli models [--profile <name>]`. Both start the ordinary
  Host and project the same schema.
- Online reconciliation retains the resolved Plan only after its Generation
  becomes active. Catalog reads pin a route, look up its Generation digest, and
  fail closed if that exact Plan authority is unavailable.

## Consequences

- Model selection is inspectable without duplicating Provider configuration or
  leaking credentials.
- A rejected desired-state edit cannot appear as the running selection, and an
  automatic rollback can return to a previously retained catalog projection.
- Adding a Provider still requires ordinary Plugin linking and Host Catalog
  metadata. The catalog does not install packages, mutate the Plugin Root, or
  claim Marketplace discovery.

## Proof

CLI parsing tests cover default and named Profile selection. A real Web Host
fixture reads the catalog endpoint and proves the selected fixture Instance and
model, typed authentication, explicit multimodal capability absence, and the
absence of credential material. Host, CLI, and Web package checks compile the
shared projection against the linked Catalog and Generation runtime.
