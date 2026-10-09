---
name: land
description: >-
  Land an explicitly requested change in LioRael/lenso-agent by verifying a
  signed candidate and pushing it normally to origin/main. Invoke only when
  the user requests landing or merging the change, not for review,
  preparation, passing checks, or installing this skill.
disable-model-invocation: true
metadata:
  delta-action: land
---

# Land lenso-agent changes

An invocation of this skill is the landing request. Proceed through verification
and the actual push without asking again whether to merge. Stop for genuine
blockers, unclear scope, or changed destination requirements.

## 1. Establish scope and destination

- Run from the current repository root. Read applicable `AGENT.md`/`AGENTS.md`,
  contribution requirements, and any new CI definitions before proceeding.
- Inspect branch, HEAD, remotes, tracked/untracked changes, and the index.
  Identify the exact files and existing commits belonging to the requested
  change. Include this skill when its installation is part of that change.
  Pause if ownership or scope is unclear.
- Preserve unrelated staged and unstaged work. Stage only resolved in-scope
  paths; do not use blanket staging, automatic stashing, resets, or cleaning.
  Generated files listed in `.gitignore`, including `dist/`, `artifacts/`,
  `node_modules/`, and temporary SQLite files, are not submission content.
- The destination is `origin/main` in `LioRael/lenso-agent`. Inspect the configured
  origin URL and accept its HTTPS or SSH equivalent only when it identifies this
  repository. Never publish through the `local` backlink.
- Confirm Git, GitHub CLI, Bun, Node, and npm are available. Check Bun against
  `package.json`'s `packageManager` and `engines`; prefer the declared version.
  Preserve the existing Git identity and SSH signing configuration. Check
  authentication through repository API access without printing credentials.
  Missing tools, signing, or permissions are blockers, not reasons to install
  tools or change credentials.
- Use `gh api` to inspect repository permissions, the current default branch,
  `branches/main/protection`, and repository rulesets. A confirmed "Branch not
  protected" response means no branch protection; an authorization or discovery
  error does not. Also inspect contribution policies and workflow files in the
  fetched destination tree. Require push permission and an unarchived repository.
  If a PR, review, merge queue, or another unsupported gate has become required,
  stop and explain that this direct-push workflow needs updating. Do not bypass
  requirements using administrator privileges.
- Fetch origin and record the current `origin/main` commit as the destination
  base. Use live refs, never a historical SHA from an earlier conversation.

## 2. Prepare a signed, isolated candidate

- Review the entire intended diff, including migrations, lockfile changes,
  public APIs, tests, and documentation. Satisfy applicable contribution
  obligations; obtain human-authored text if a policy requires it. Do not invent
  a Conventional Commit, issue, CLA, or review requirement when none exists.
- Run `git diff --check` on the intended diff. If uncommitted in-scope work
  remains, stage its explicit paths and create a signed commit with a concise
  imperative subject. Use `git commit --only` with those same explicit paths
  so unrelated staged changes cannot enter the commit. Inspect the resulting
  commit and verify that unrelated index entries and working files were
  preserved. A signing failure must stop; never disable signing to proceed.
- Prefix commands that could open an editor with `GIT_EDITOR=true` and provide
  a commit message or `--no-edit`. Keep signing enabled for commits and merges,
  using `git -c commit.gpgsign=true` when necessary.
- Create a uniquely named private landing branch and clean temporary Git
  worktree beneath this repository's ignored `artifacts/` directory, starting
  from the source candidate. Record ownership of that exact branch and path.
  Perform upstream integration, conflict resolution, and verification there,
  leaving the original checkout and unrelated work alone.
- Merge the recorded `origin/main` into the landing branch with a non-interactive,
  signed merge. Prefer a fast-forward where Git can do one; do not rebase or
  rewrite shared history.
- Resolve clear conflicts automatically when both intent and the combined result
  are evident. Preserve unrelated upstream changes. Pause on ambiguous behavior,
  conflicting ownership, unsafe migrations, or uncertain dependency/lockfile
  reconciliation. An unresolved conflict means the change has not landed.
- Record the resulting candidate SHA. If it is already an ancestor of the
  destination and there is no remaining requested diff, verify that state rather
  than making an empty commit or duplicate push.

## 3. Verify the exact candidate

Run the following in the clean candidate worktree. Inspect current definitions
before running them; if definitions changed, establish the equivalent required
checks rather than silently dropping verification.

| Check | Invocation | Repository source |
| --- | --- | --- |
| Frozen dependency installation | `bun install --frozen-lockfile` | `package.json` `packageManager`/`engines`, `bun.lock`; Bun install's supported frozen-lockfile flag |
| Tests | `bun run test` | `package.json` `scripts.test` runs `bun test tests` |
| Typecheck | `bun run typecheck` | `package.json` `scripts.typecheck`, `tsconfig.json` |
| Build and declarations | `bun run build` | `package.json` `scripts.build`, `tsconfig.build.json` |
| Offline runtime workflow | `bun run example` | `package.json` `scripts.example`, `examples/offline.ts` |
| Framework artifact integration | `bun run check:consumer` | `package.json` `scripts.check:consumer`, `scripts/packed-consumer.ts` snapshot branch |
| Ordinary Bun consumer | `bun run check:consumer:ordinary` | `package.json` `scripts.check:consumer:ordinary`, `scripts/packed-consumer.ts` `--ordinary` branch |
| Ordinary npm consumer | `bun run check:consumer:ordinary --npm` | Same script; `scripts/packed-consumer.ts` explicitly handles `--npm` and performs npm install/ci |
| Local package candidate | `bun run pack` | `package.json` `scripts.pack`; local packing only |

- Consumer checks rebuild the same `dist/` directory. Run build/consumer/pack
  operations serially; do not race their cleanup steps.
- Run whitespace verification against the complete destination-to-candidate
  diff, and confirm no tracked candidate files or lockfile changed during checks.
  If verification changes tracked content, investigate and produce a new signed
  candidate; rerun all required checks on it.
- All listed checks and any applicable required remote checks must have completed
  successfully for this exact candidate before pushing. Pending, missing,
  failing, or unverifiable required results are blockers. Discover current
  check requirements again before landing; do not rely on earlier test counts.
- The normal typecheck follows `tsconfig.json`; consumer declaration checking
  follows `scripts/packed-consumer.ts`. The optional `--check-dependency-types`
  diagnostics documented in README's "Local artifacts and checks" section are
  an accepted upstream limitation, not a required check for this workflow.
  This exception does not waive failures in any listed check or override a new
  contribution or destination rule that requires stricter declaration checking.
- Tests and examples use local fixture data. Do not call paid models, use
  production data, execute production migrations, or remove credential and
  loopback safeguards to make a test pass. Keep Console/native packaging and
  upstream Pi repairs outside the requested landing scope.

## 4. Land and verify

- Fetch origin again and compare `origin/main` with the recorded destination
  base. If it advanced, merge the new tip into the clean landing branch, resolve
  clear conflicts under the same policy, and rerun the complete required checks
  on the new candidate. Recheck applicable destination rules.
- Confirm the candidate descends from the current `origin/main`, its requested
  changes are reviewed, its commits are signed, and every required check is
  successful. Push the candidate with a normal explicit refspec:
  `git push origin HEAD:refs/heads/main`. Never force-push, bypass protection,
  enable automatic merging, or alter repository permissions.
- If the push is rejected because the destination advanced, fetch and repeat
  integration and verification. A rejection for permissions or a new required
  workflow is a blocker; do not attempt an alternative bypass.
- Fetch the destination and verify the candidate is an ancestor of its current
  main tip. Independently check GitHub's main commit or compare API. Landing is
  complete only when the intended commit is demonstrably contained in remote
  main; a local commit, prepared branch, or started check is not completion.
- Remove only the temporary worktree created for this run, and only when safe.
  Use ordinary worktree removal; preserve it for diagnosis if removal would
  discard unexpected modifications. Delete the owned local landing branch only
  when normal safe deletion succeeds; never force cleanup of unrelated work.
- State the landed commit and destination, checks performed, retained limitations,
  and any remaining cleanup. On a blocker, state that the changes have not landed
  and identify the concrete next step.
