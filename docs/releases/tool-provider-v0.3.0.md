# Lenso Agent Tool Provider 0.3.0

This is a new Rust compatibility cohort. `lenso-capability-agent-tool-provider`
0.3.0 uses the released `lenso-runtime-codec` 0.4.x and `lenso-guest-sdk`
0.5.x APIs. Its public `ToolProviderJsonCodec` and `ToolProviderGuestClient`
therefore cannot substitute for their 0.2.x counterparts in a consumer still
using Codec 0.3.x or Guest SDK 0.4.x. The Capability identity
`lenso.agent.tool-provider@2`, Descriptor version 2.1.0, Operations, and six
Schemas are unchanged; this is a Rust API compatibility break, not a wire
contract change.

The workspace also bumps `lenso-agent-tool-sdk` and its macros to 0.4.0 so
its local path dependency and public re-export compile against Provider 0.3.0.
Those two versions are source compatibility inputs for the present
`provider-only` release selection; that selection does not publish them. Tool
SDK 0.3.3, macros 0.3.3, and Provider 0.2.2 remain the currently published
versions until separately authorized releases occur. A future `sdk-only`
selection checks that Provider 0.3.0 already exists before uploading macros
0.4.0 and SDK 0.4.0; `sdk-cohort` retains the original three-package path.

Push the reviewed commit to a unique `codex/verify/**` candidate ref. The
repository `quality` workflow and the dedicated
`release-tool-sdk-crates.yml` verification run for the same SHA. Confirm both
successful runs before landing that exact commit. The dedicated workflow tests
and packages the closed source cohort, checks all three normalized archive
manifests, then compiles the packaged Provider against registry-only Codec 0.4.1. A
diagnostic `publish: false` dispatch accepts a candidate ref only when
`revision` matches its checked-out SHA.

Provider publication requires a separate `release-tool-sdk-crates.yml` dispatch
from `main` with `release_set: provider-only`, the full 40-character reviewed
commit as `revision`, version `0.3.0`, and `publish: true`, after separate
authorization for this exact public version. The workflow rejects a dispatch
if `main` has moved past that revision.
Immediately before its one upload, the publish step requires live `origin/main`
and the GitHub API's `main` to match the checked-out SHA and repository identity.
Configure Trusted Publishing for Provider 0.3.0 to trust this workflow. The job
uploads only Provider 0.3.0, then downloads that exact crate, checks its Cargo
VCS commit against the dispatched SHA and its checksum against a fresh crates.io
lockfile, and compiles a new external consumer with an exact Provider pin and no
path or Git patches. Only successful readback and consumer check establish
registry visibility. The ref check and upload are not atomic; if the status is
uncertain, inspect the exact registry version before any retry.
A passing local package test, candidate CI, or publication dispatch alone is
not registry visibility proof.

The `sdk-only` and `sdk-cohort` selections remain available for independently
authorized SDK publication. They are not part of the Provider-only release.
Before a CLI adopts these versions as an installable default, run the
[cross-repository starter prerequisite gate](../tool-starter-release-prerequisites.md).
Matching source versions and a Provider-only release do not establish that the
Rust or TypeScript Tool SDK is available to a new registry-only consumer.

The current development workspace still patches Codec to a Git revision, so
Cargo's automatically included package lock records that source. The
registry-only check deliberately resolves a fresh lock from the packaged
manifest, as ordinary downstream library consumers do. It proves that
registry-only consumption compiles; it does not claim that an unpacked archive
using its included lock is Git-free.
