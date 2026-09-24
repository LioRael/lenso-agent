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

Push the reviewed commit to a unique `delta/verify/**` or `codex/verify/**`
candidate ref. The repository `quality` workflow and the dedicated
`release-tool-sdk-crates.yml` verification run for the same SHA. Confirm both
successful runs before landing that exact commit. The dedicated workflow tests
and packages the closed cohort, checks all three normalized archive manifests,
then compiles the packaged Provider against registry-only Codec 0.4.1. A
diagnostic `publish: false` dispatch accepts a candidate ref only when
`revision` matches its checked-out SHA.

Publication requires a separate `release-tool-sdk-crates.yml` dispatch from
`main` with the full 40-character reviewed commit as `revision`, version
`0.4.0`, and `publish: true`. The workflow rejects a dispatch if `main` has
moved past that revision. Immediately before each package upload, the publish
step requires live `origin/main` and the GitHub API's `main` to match the
checked-out SHA and repository identity. Re-review and re-verify if `main` has
advanced. The workflow publishes Provider 0.3.0, macros 0.4.0, then SDK 0.4.0
with separate commands. Configure Trusted Publishing for all three crates to
trust this workflow. The publish job then downloads each exact
crate, checks its Cargo VCS commit against the dispatched SHA and its checksum
against a fresh crates.io lockfile, and compiles a new external consumer with
exact version pins and no path or Git patches. Only a successful readback and
consumer check establish registry visibility. Coordinate a short period in
which `main` stays fixed while publishing. The ref check and an individual
upload are not atomic; a change during one upload may still allow that upload
to finish, but the next package is not attempted if the next check fails.
After a timeout or partial failure, inspect each exact registry version before
deciding whether and how to resume; do not blindly redispatch.
A passing local package test, candidate CI, or publication dispatch alone is
not registry visibility proof.

The current development workspace still patches Codec to a Git revision, so
Cargo's automatically included package lock records that source. The
registry-only check deliberately resolves a fresh lock from the packaged
manifest, as ordinary downstream library consumers do. It proves that
registry-only consumption compiles; it does not claim that an unpacked archive
using its included lock is Git-free.
