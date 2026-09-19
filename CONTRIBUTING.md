# Contributing to Lenso Agent

This guide is the tool-, editor-, and Delta-independent entry point for human
contributors. It describes how to hand off a change for maintainer review; it
does not grant repository, workflow, package, or release permissions.

## Handoff a change

1. Fork `https://github.com/LioRael/lenso-agent`, create a focused branch, and
   push your work there. An Issue is the handoff record: include a problem
   summary, the fork URL and branch, the immutable full 40-character commit
   SHA, focused validation run (including versions), and known limitations.
2. Keep the change reviewable. Do not include credentials, generated secrets,
   unrelated formatting, or changes outside the stated problem. The maintainer
   may ask for a fresh SHA after review comments.
3. If a fork push is not practical, create a durable patch instead:
   `git format-patch --binary --full-index --stdout <base>..<sha> > change.patch`.
   Attach the patch and the same problem, SHA, validation, and limitation
   record to the Issue. Do not rely on an editor diff, chat transcript, or a
   mutable branch name as the handoff.

Maintainers review and import the submitted revision into this repository only
after checking its scope and validation. Treat workflow files, shell scripts,
release configuration, and dependency changes as untrusted input: inspect them
before execution, use least-privilege credentials, and never run commands that
publish, deploy, or disclose secrets merely to review a contribution.

## Review and delivery

The normal delivery proof is candidate-first. A maintainer rebases or imports
the reviewed change onto the current `origin/main`, records the full base and
candidate SHAs, and pushes one unique `delta/verify/lenso-agent/<attempt>` ref.
The `quality` workflow must be triggered by that candidate push and its
`quality` job must succeed for the exact candidate SHA and attempt. A successful
local check or manually dispatched run is not a substitute. After confirming
that `main` has not advanced, the maintainer fast-forwards the exact verified
SHA to `main` and reads back remote `main`; a race requires a new candidate.
Candidate refs are cleaned up after the result is recorded.

`Delta Land Changes` is Delta's managed delivery path. `/land` is its optional
workflow entry point, not a universal shell command or a permission grant.
Other agents may use their own reviewed delivery tooling; plain Git users use
the fork/Issue or format-patch handoff above and the same immutable-revision
review contract. None of these names bypass review, branch protection,
authorization, or the candidate gate.

## Workflow boundaries

`.github/workflows/quality.yml` runs the named static, tests, contracts, and
marketplace-installation jobs only for candidate `delta/verify/**` pushes or
an explicit diagnostic dispatch. The expensive external workflows retain their
narrow lifecycle: `marketplace-remote.yml` and `projects-acceptance.yml` run
for their relevant pull-request paths or explicit dispatch, while
`reconcile-benchmark.yml` is explicit-dispatch only. Prose-only edits do not
start those external acceptance or benchmark jobs.

Release workflows remain separate. Tag/manual release workflows, package
selection, pinned actions, OIDC, and product identity checks are unchanged.
Candidate pushes cannot publish packages, create releases, deploy, or mint
release tags. Publication requires its existing explicit authorization and
release boundaries; a candidate quality run is only validation evidence.
