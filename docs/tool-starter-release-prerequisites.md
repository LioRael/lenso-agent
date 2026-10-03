# Verify the default Tool starter before promoting a CLI release

`lenso plugin new` belongs to the Rust CLI repository. Its default Agent Tool
templates depend on SDKs released from this repository. A source version bump,
a package build, and registry availability are three different facts. Changing
one repository's version does not publish the other repository's dependencies.

Use the read-only gate with an exact Lenso checkout and an already available
`lenso-contract-codegen` binary. Python 3.11+ is sufficient; the gate never runs
Cargo, installs packages, executes package scripts, or publishes anything:

```sh
python3 scripts/check-tool-starter-prerequisites.py \
  --lenso-root /path/to/reviewed/lenso \
  --codegen /path/to/lenso-contract-codegen > prerequisites.json
```

The report records both Git SHAs and the codegen binary SHA-256. Run from clean
reviewed checkouts; dirty worktrees are recorded explicitly. Relevant workspace
patches are listed without substituting them for registry evidence.
`--offline` skips every registry request and still generates
the committed contract in a temporary directory; it cannot establish release
availability. No local Cargo/npm cache is substituted for an online observation.

| Exit | Meaning |
| --- | --- |
| 0 | Checked template/source identities, declared API and public package prerequisites agree. |
| 1 | At least one concrete blocker: pin/API drift, missing or yanked version, invalid metadata, or archive mismatch. Other rows may still be inconclusive. |
| 2 | No concrete mismatch was found, but availability is unverified because of offline mode, access, transport, rate limiting, or server failure. |

HTTP 200 package metadata with no matching version is `version_absent`.
If matching Cargo versions exist but all are yanked, the state is
`version_yanked`: these versions remain occupied and cannot be uploaded again.
`observed_versions` includes yanked versions; `usable_versions` excludes them.
HTTP 404 is `package_not_found`; HTTP 401/403 is `access_error`; 429 is
`rate_limited`. DNS/TLS/timeouts and other transport errors remain separate
unknown states. The gate does not echo proxy URLs, authentication headers or
private exception messages, and does not retry requests automatically.

## What the gate proves

- Read the actual Rust/Bun template requirements, including their shared
  constants, and compare them with this repository's package manifests.
- Check SDK → macros/Provider source versions and the template's generic
  Rust/Bun Plugin SDK cohort.
- Regenerate the committed Descriptor/Schemas with the selected codegen and
  compare the resulting digest, identity and Request operations with npm build
  metadata and Rust/TypeScript projections. Check declared source exports,
  including the Rust SDK's current explicit public re-export layout. This
  bounded source check fails on changed layouts and is not a Rust parser.
- Query official public registry metadata for the exact cohort packages, the
  direct first-party Rust normal/build prerequisites, and direct npm build and
  runtime dependencies. This bounded gate is not a replacement Cargo/npm solver.
- For present cohort archives, download with a size bound, verify the registry
  checksum, normalized identity, and SDK dependency/API metadata. The published
  Provider Descriptor and six Schemas must equal the reviewed source snapshots.

Version presence alone does not establish a compilable Rust ABI or a runnable
Bun starter. The existing release workflows still own package compilation,
tests, exact reviewed archives, and registry-only consumption. This gate does
not grant publication authorization, assess private source overrides, or supply
CI evidence for a candidate it has not run.

## Current source cohort and publication order

At Agent source `25b0741657f67b7137aba6bfc2ed6d2ee2086b32`, the intended cohort is:

| Package | Source version | Dependency / role |
| --- | --- | --- |
| `lenso-capability-agent-tool-provider` | 0.3.0 | Codec 0.4.x / Guest SDK 0.5.x Rust API; unchanged Tool Provider wire contract |
| `lenso-agent-tool-sdk-macros` | 0.4.0 | Publish before the Rust SDK |
| `lenso-agent-tool-sdk` | 0.4.0 | Depends on macros 0.4.0 and Provider 0.3.0 |
| `@lenso/agent-tool-sdk` | 0.1.1 | Uses Bun Plugin 0.4.2, Contract Runtime 0.3.1 and the supported `@lenso/bun` 0.5.3 facade |

The CLI source at `73417e9f85a21f5e0d4b9658de45db2434391ce8`
already selects Rust `^0.4.0` and Bun `0.1.1`. These match the source manifests;
they are not a spelling error. The [Provider release note](releases/tool-provider-v0.3.0.md)
explicitly explains that the Rust SDK bumps are source compatibility inputs and
`provider-only` does not publish them. Do not interpret a Provider release or
candidate CI as SDK publication.

The 2026-10-02 public check found Provider 0.3.0 downloadable with a matching
wire contract, while macros 0.4.0, Rust SDK 0.4.0 and npm SDK 0.1.1 were absent.
Recheck before any owner action; these are observations, not permanent facts.
The owner sequence for that state is:

1. Review and qualify the exact current source. Confirm first-party runtime
   prerequisites and Provider 0.3.0 are still present. Do not republish an
   occupied Provider version or choose `sdk-cohort` for this state.
2. Use `release-tool-sdk-crates.yml` verification with `release_set: sdk-only`,
   `version: 0.4.0`, the exact selected revision, and `publish: false`. A later,
   separately authorized publish dispatch must run on the exact current main
   required by its existing gate. Its `sdk-only` order is macros → SDK.
3. Require the workflow's `published-sdk-only` readback: published Provider,
   macros and SDK, correct registry checksums/provenance, and an unpatched
   external Rust consumer. On partial/unknown publication, inspect exact
   versions before resuming; do not automatically repeat uploads.
4. Independently run `release-agent-tool-sdk-npm.yml` with the selected
   `source_sha`, `version: 0.1.1` and `mode: dry-run`. Review the archive SHA256.
   Any later authorized publish uses that hash, `mode: publish`, and the
   existing explicit confirmation. Preserve all current OIDC/main/version gates.
5. Before presenting the default CLI starter as installable, rerun this
   prerequisite gate online and qualify unchanged Rust and Bun starters using
   registry dependencies, without local/Git substitutions.

The Rust and npm publication tracks can be reviewed independently; both must
close before claiming the two default authoring paths are ready. Old SDKs are
not a silent fallback. Rust Provider 0.2.x has a different public compatibility
cohort; the older npm SDK's matching wire digest alone does not prove its older
Bun dependencies are compatible with the current starter.

Offline regression coverage:

```sh
python3 scripts/test-tool-starter-prerequisites.py
```

These tests run in both existing SDK release workflows. The cross-repository
online gate is explicit because it requires a reviewed CLI checkout; it does
not silently fetch a moving main branch from another repository.
