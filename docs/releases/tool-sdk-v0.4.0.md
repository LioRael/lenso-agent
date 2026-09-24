# Lenso Agent Tool Provider 0.3.0 and Tool SDK 0.4.0

This is a new Rust compatibility cohort. `lenso-capability-agent-tool-provider`
0.3.0 uses the released `lenso-runtime-codec` 0.4.x and `lenso-guest-sdk`
0.5.x APIs. Its public `ToolProviderJsonCodec` and `ToolProviderGuestClient`
therefore cannot substitute for their 0.2.x counterparts in a consumer still
using Codec 0.3.x or Guest SDK 0.4.x. The Capability identity
`lenso.agent.tool-provider@2`, Descriptor version 2.1.0, Operations, and six
Schemas are unchanged; this is a Rust API compatibility break, not a wire
contract change.

`lenso-agent-tool-sdk` 0.4.0 publicly re-exports the 0.3.0 Provider types and
requires `lenso-agent-tool-sdk-macros` 0.4.0. Previously published Tool SDK
0.3.3 and Provider 0.2.2 remain available for existing consumers. Examples
which exercise that released baseline intentionally retain their 0.2.2
requirements until the new cohort is visible in the registry.

Before publication, run `release-tool-sdk-crates.yml` from the exact reviewed
`main` commit with version `0.4.0` and `publish: false`. The workflow tests and
packages the closed cohort, then compiles the packaged Provider against
registry-only Codec 0.4.1. Publication requires a separate dispatch with
`publish: true`; it publishes Provider 0.3.0, macros 0.4.0, then SDK 0.4.0.
Configure Trusted Publishing for all three crates to trust this workflow.
Read back each exact crate from the registry and compile an external consumer
without path or Git patches before claiming the release complete. A passing
local package test, candidate CI, or publication dispatch is not registry
visibility proof.

The current development workspace still patches Codec to a Git revision, so
Cargo's automatically included package lock records that source. The
registry-only check deliberately resolves a fresh lock from the packaged
manifest, as ordinary downstream library consumers do. It proves that
registry-only consumption compiles; it does not claim that an unpacked archive
using its included lock is Git-free.
