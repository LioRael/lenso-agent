# Land Lenso changes

Use this optional agent entry point with [`CONTRIBUTING.md`](../../../CONTRIBUTING.md).
It does not grant permission to land, publish, release, or deploy. In Delta,
`Delta Land Changes` and `/land` are managed delivery names; `/land` is not a
universal shell command.

## Candidate-first manual path

1. Read `AGENTS.md`, check status/diff/remotes, preserve unrelated work, and
   obtain review for the final diff. Record the full `origin/main` base SHA
   after review fixes.
2. Run focused checks for the changed files. Push the final commit once to a
   unique ref:
   `git push origin <candidate-sha>:refs/heads/delta/verify/lenso-agent/<attempt>`.
3. Accept only the `quality` workflow run caused by that push when repository,
   workflow, event, ref, exact `head_sha`, attempt, and the `quality` job all
   match and succeed. Local or manually dispatched runs do not substitute.
4. Fetch `origin/main` again. If it advanced and the candidate is not an
   ancestor, integrate, review, and create a new candidate. Otherwise push the
   exact verified SHA to `refs/heads/main`, read it back, and verify ancestry.
5. Record review evidence, base/candidate/landed SHAs, run URL and attempt,
   then remove the candidate ref.

Maintainers still own authorization, branch protection, delegated-user and
Agent lifecycle boundaries, marketplace evidence, package publication, release
selection, and deployment. Never turn a candidate validation push into a
publication or release operation.
