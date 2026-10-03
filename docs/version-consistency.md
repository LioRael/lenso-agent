# Check selected source versions and release prerequisites

`scripts/check-version-consistency.py` collects read-only evidence from explicit,
clean Core, JS, Agent and Console checkouts. It reuses the Tool starter registry
and bounded SemVer checks, plus repository-owned source/projection verifiers.
It never fetches source, installs tools, compiles a workspace, changes versions,
publishes packages, dispatches CI, or grants release authorization.

Packages retain independent versions. A source version bump can legitimately
precede publication. Missing source-only versions are pending release evidence;
an unavailable actual consumer requirement blocks that consumer. Only existing
contracts that explicitly require an exact source pair enforce one. In
particular, the CLI archive binds JS and Rust sources; ordinary JS packages do
not have to share a revision with an arbitrary Core checkout.

Agent examples explicitly documented as source-local and naming temporary
`[patch.crates-io]` inputs in their `verify-foundation.py` retain a separate
`source_fixture` requirement scope. Missing public versions remain pending
released-consumer qualification, not proof that the source-local recipe failed.
The verifier is parsed only to record explicit patch names; it is not run,
and its framework layout or API compatibility is not qualified here.
The independent `agent-foundation-lifecycle/verify.py` has a bounded adapter
for its current literal Core package list and runner template's Agent package
patches. A changed patch-construction shape blocks input processing until
reviewed; arbitrary Python is never interpreted. Its lifecycle execution is
a separate required Foundation CI check, not a registry availability result.

Console packager outputs are source-version observations until a produced
launcher is selected for consumption. Their absence does not instruct an owner
to publish every possible distribution variant. Agent binary archive bytes and
execution remain outside this metadata gate, explicitly listed in `not_proven`.

## Run with reviewed inputs

Use Python 3.11+, the existing Node executable, and an already available codegen.
Write a selection file with the real absolute checkout paths and **full** reviewed
SHAs. For example, this shape intentionally uses placeholders, not a runnable
claim about a moving branch:

```json
{
  "schema": "lenso.version-consistency-selection@1",
  "repositories": {
    "core": {"path": "/reviewed/core", "revision": "FULL_REVIEWED_CORE_SHA"},
    "js": {"path": "/reviewed/js", "revision": "FULL_REVIEWED_JS_SHA"},
    "agent": {"path": "/reviewed/agent", "revision": "FULL_REVIEWED_AGENT_SHA"},
    "console": {"path": "/reviewed/console", "revision": "FULL_REVIEWED_CONSOLE_SHA"},
    "console_core": {"path": "/reviewed/console-core", "revision": "CONSOLE_QUALIFIED_CORE_SHA"}
  },
  "documents": {
    "core": ["README.md"],
    "js": ["README.md"],
    "agent": ["README.md", "docs/foundation-sdk-release.md"],
    "console": ["README.md", "docs/release-process.md"]
  },
  "pending_candidates": []
}
```

`console_core` is the exact Core checkout selected by Console's qualification
workflow. It may differ from the separately selected Core CLI checkout. The
outer gate checks its actual Git identity before invoking Console's checker.
Do not overwrite that declared combination with another convenient checkout.
If a requested candidate is unavailable, retain an explicit entry such as
`{"repository":"core","reference":"055ea6c3","reason":"No matching visible full ref was available"}`
in `pending_candidates`; do not replace it with cached main and call it qualified.

```sh
python3 scripts/check-version-consistency.py \
  --selection /path/to/reviewed-selection.json \
  --codegen /path/to/lenso-contract-codegen > consistency.json
```

`--offline` skips all registry and GitHub release metadata requests. It still
reads source, checks projections and writes temporary Console result records;
it cannot produce an availability pass. No repository, kit or generated source
is written. Online mode uses official public endpoints, four workers and one
fresh response per URL within the run; no persistent/package cache is evidence.

## What the report checks

| Input | Rule and existing owner |
| --- | --- |
| Selected repositories | Full requested SHA, actual HEAD, clean worktree and expected repository identity. Equal package versions never substitute for equal source identity. |
| Tracked manifests | Package inventory with independent versions and publication policy; explicit local path requirements must admit their target package version. Workspace/Git/path sources remain distinguished from registry dependencies. |
| Prepared locks | Direct first-party Cargo requirements have at least one compatible resolution in their package/workspace lock. Cargo membership follows the declared members/exclusions and local path members; unsupported ownership stays unknown. Bun checks declared members are recorded, then compares each importer's manifest and direct resolution; ambiguous nested resolution stays unknown. This is bounded static validation, not a complete solver or proof that optional/feature graphs resolve. |
| Tool starter | Reuse `check-tool-starter-prerequisites.py` for Core template/Agent SDK versions, declared public API, generated contract identity and selected published archives. |
| Core Web template | Call `check_web_cohort` from Core's existing `.github/scripts/check-fixture-inputs.py`, without calling its Cargo lock/build phase. |
| JS projections | Run `packages/lenso-bun/scripts/capabilities.mjs check` against the selected codegen, never `sync`. The lock records independent upstream source revisions; the check validates local snapshots/projections, not their upstream provenance. |
| Agent binary cohort | Run the existing `scripts/check-release-version.sh`: only its four binary versions and installer default must agree. Read explicit CI codegen/CLI install pins as additional registry requirements. |
| Console SDK/source | Run `tooling/check-console-sdk.mjs` without `--write`. Reuse the selected candidate's `check-development-host-source.py` for facility and, when supplied, generated runtime inputs. Its temporary `--record` is required; `--kit` is never passed. |
| Console distribution | Respect the existing packagers' deliberate replacement of private launcher template versions with `apps/shell/package.json`'s version. Check the separately pinned Agent release's tag/asset metadata; archive bytes remain unverified. Do not equate Console's application version with Agent's binary version. |
| Configured active docs | Check local Python/Node/shell entry files, npm scripts, and Bun scripts/local files in simple shell fences. Literal `cd` is tracked. Unknown Bun bin providers, incomplete fences/continuations, variable/complex shell and other commands remain explicitly unknown/unassessed, never executed. Historical docs are not swept into this scope implicitly. |
| Public registry | Reuse the original official-registry transport, archive checks and absent/yanked/network classifications. Unsupported requirement syntax is unknown, never guessed. Metadata availability is not ABI compatibility. |

Cargo locks may legitimately contain multiple versions and Git/registry entries.
The scanner does not impose global uniqueness. Console's generated Host has a
stricter existing rule for **nine runtime identities**, enforced only by its own
runtime checker. Historical/current codegen versions may coexist.

The report includes `sources`, `package_inventory`, `registry` and structured
`checks` with status, locations and remediation. Exit codes:

- **0**: every supplied/required check in this bounded scope passed; publication,
  full resolution and native/portable execution are still not proved.
- **1**: at least one concrete mismatch or unavailable consumer prerequisite.
- **2**: no concrete mismatch, but some source/artifact/network/document coverage
  remains unknown. Offline availability is always unknown.

## Supply existing artifact evidence when available

For the exact CLI source pair, add `cli_candidate` with the paths/values already
produced by `.github/scripts/cli-release-artifacts.mjs` in the selected JS repo:

```json
{
  "archive": "/reviewed/candidate/lenso-cli-VERSION.tgz",
  "manifest": "/reviewed/candidate/manifest.json",
  "artifact_metadata": "/reviewed/public-artifacts.json",
  "run_id": "REVIEWED_RUN_ID",
  "run_attempt": "REVIEWED_ATTEMPT",
  "reviewed_archive_sha256": "sha256:REVIEWED_DIGEST",
  "candidate_artifact_id": "REVIEWED_ARTIFACT_ID",
  "candidate_artifact_digest": "sha256:REVIEWED_ARTIFACT_DIGEST"
}
```

The existing `verify-candidate` implementation checks archive bytes, manifest,
four native receipts and public artifact identities. Missing artifacts stay
pending; source/manifest checks cannot replace them. Supplying API JSON here is
evidence replay, not a fresh GitHub authentication or CI-status proof.

For Console runtime provenance, `console_generated_seed` points to the existing
reviewed seed containing `.lenso/generated-host/` and `.lenso/distribution.lock.json`.
No Stream test or seed build runs. Without it, runtime qualification stays pending.

## CI and regressions

Agent's existing `scripts/check.sh preflight` runs the pure offline regressions:

```sh
python3 scripts/test-tool-starter-prerequisites.py
python3 scripts/test-version-consistency.py
python3 scripts/test-version-manifest-inputs.py
python3 scripts/test-version-doc-entrypoints.py
```

A caller that already prepares reviewed cross-repository checkouts can run the
report command as an additional CI step and retain its JSON artifact. It must
honor the exit code; `2` is not success. This patch does not add implicit fetches,
publish permission or a new release workflow. The ordinary regression CI result
does not qualify unavailable candidate SHAs or unpublished dependencies.
