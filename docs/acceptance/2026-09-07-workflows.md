# Agent and Console workflow acceptance — 2026-09-07

Verdict: local source-cohort acceptance passes for the supported workflows below.
Release acceptance remains pending: these fixes have not been published, and a
released Agent must consume the updated Wasm Adapter before the portable Bundle
workflow can be claimed for installed versions.

## Scope and authority

The standard Console launcher retains SQLite configuration management. It no
longer advertises named Profile selection under that authority. Coding uses an
explicit local-authority Home, installed coding Profiles, and the five explicit
Tool grants `read,edit,run_process,checkpoint_create,checkpoint_accept`.
Live Profile import into SQLite/remote management is not implemented or claimed.

All live tests used isolated Agent Homes and a disposable calculator workspace.
The existing authentication was referenced through its Auth Plugin without
reading or logging credential contents. No production App was changed. Test
servers were stopped, and temporary Cargo path patches were removed afterward.

## Original failures and fixes

| Failure | Fix and ownership | Verification |
|---|---|---|
| Profile request returns 404 and errors disappear in existing chats | Console forwards the authorized App control route and shows runtime errors for existing chats and the quick panel. Launcher accepts explicit Profile and Tool grants. Agent reports authority-compatible Profile capability. | Local control request returns the selected `code` Profile. A deliberately missing `plan` Profile produces a visible error while the prior conversation and Auto selection remain. Managed bootstrap reports `profileSelection=false`. |
| Offline coding installer corrupts managed desired/materialized agreement | Agent Host records a presence-only managed-configuration guard; CLI checks it and legacy default SQLite stores before migration or writes. | Default-store and custom-authority guard regressions pass. Live managed installation rejects before creating Profile files; management still returns HTTP 200. |
| Plugin page stays in Loading after management failure | Console gives query errors precedence over a simultaneously pending query. | A controlled materialization mismatch displays “Plugins unavailable” with the conflict. Restoring the isolated file returns HTTP 200 and all 27 Plugin rows. |
| Parallel approval-backed Tools fail with ResourceExhausted | Agent Host assigns bounded, serial Hook admission to the Tool consumers. | A deterministic HTTP test emits two calls in the same model step, obtains two distinct approvals, and completes both. The real model repeats the original parallel-read scenario successfully. |
| Portable example does not compile or complete its install lifecycle | Example includes optional content blocks and documents explicit Instance configuration. CLI composes identical dependency-free v1/v2 Contracts, preserves Instance ordering/resources during configuration publication, and disambiguates release version flags. Wasm Adapter accepts the dependency-free v2 Contract with its existing request profile. | Both implementations build; Process executes directly; the combined Bundle installs, configures, runs, disables/enables, upgrades, rolls back, and removes successfully. |

## Real coding proof

Session `084d6f34-a3aa-4264-97a4-1bc61ed226a2` used `gpt-5.6-luna`.
The prompt asked the Agent to inspect README.md and calculator.py, fix addition,
validate with `python3 check.py`, and accept the checkpoint. It did not request
serial Tool calls. The model emitted both reads in one step; both completed
through independent Web approvals. It created checkpoint
`180f8a21-9ead-4d8c-8976-833f2bc8bb10`, changed subtraction to addition, received
exit 0 with `ACCEPTANCE_CHECK_PASSED`, accepted the checkpoint, and persisted
`turn_completed`.

The checker was unchanged; SHA-256:
`7513803cb03e196d6fa81876aca836988ed3f423b87d30c210cd467da09327e7`.
Local detailed evidence is retained in the Console acceptance worktree under
`output/acceptance/coding-events.json` and its isolated session database.

## Portable Bundle proof

The installed baseline CLI was 0.4.8 and hung on Process v2 discovery. An isolated
published CLI 0.5.1 still reproduced the authoring-version mismatch. Acceptance
then used the fixed CLI source and a temporary Agent dependency override to the
fixed Wasm Adapter; it did not replace the user's global CLI.

The final Bundle used the existing published Plugin SDK 0.4.5. The exploratory
SDK descriptor change was discarded; its wire format is unchanged.

| Stage | Evidence |
|---|---|
| Pack 1.0.0 | Manifest digest `sha256:c48800b8bc662fdf75011609322827aef4bb2c6a240c7d6e95ffb3b9a115b9d3`; contains Wasm and Process artifacts |
| Install/configure/call | Session `1ff30434-3ab2-49cd-bf6b-d71931ea2644`, result `LENSO PLUGIN` |
| Disable | An explicit uppercase grant is rejected because the Tool is outside the Plan-bound catalog |
| Enable and upgrade | Local loopback catalog upgrades to 1.0.1; session `ade13d76-e697-46cf-a4a0-e1417372dc95` returns `LENSO PLUGIN` |
| Rollback | History retains both versions; rollback to 1.0.0 succeeds; session `6fa45c11-dfb4-4f07-b58a-23403c18c890` returns `LENSO PLUGIN` |
| Remove | Plugin is moved to recoverable trash; ordinary no-tool session `1a18290c-ddb4-4670-bcf9-365bff5aeada` completes |
| Process implementation | Direct `plugin dev --implementation process` returns `LENSO PLUGIN` |

No public catalog entry or package was published. The 1.0.1 artifact exists only
for this local upgrade test; example source version is restored to 1.0.0.

## Regression checks

- Console: 221 local tests, 35 browser tests, 10 service tests; lint, TypeScript,
  production API-mode build, Rust formatting and Clippy pass.
- Agent: 110 Host unit tests pass (one pre-existing ignored test), eight CLI
  Profile tests, 81 Web unit tests, nine Web HTTP tests; targeted Clippy passes.
- CLI: 52 library tests and 53 binary tests pass (five pre-existing clean-room
  tests ignored); configuration publication regressions also cover resources.
- Runtime: all six real Wasm Component integration tests pass, including v2
  admission, invocation, streams, host imports, trap/cancellation and bundle
  descriptor checks; targeted Clippy passes.

Browser tests exited successfully but reported an existing teardown timeout;
macOS linking emitted an unwind-table size warning. Neither failed a check.

## Delivery gate

Review the four owning repository changes together. The Runtime Adapter and CLI
must be released through their repository workflows, then the Agent dependency
cohort must be updated and retested against registry artifacts. The Console
change includes a Changeset. Local passing evidence does not mean the current
published installers or default managed Home support live Profile import.
