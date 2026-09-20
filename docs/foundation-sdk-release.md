# Foundation SDK releases

The Foundation SDK cohort contains Model, Turn Processing, Extension State,
Interaction, Dynamic Authority, and Durable Task. Each owns a public Capability
API and is published independently on crates.io. Turn Processing depends on
Model; Cargo's multi-package publisher respects that dependency order.

Only these six packages enable `publish = ["crates-io"]`. The rest of the
Agent workspace keeps its existing publication policy.

Validate the cohort from a clean checkout:

```sh
bash scripts/foundation-sdk-packages.sh test
bash scripts/foundation-sdk-packages.sh verify
```

Candidate pushes run both the repository quality gate and the dedicated
`Release Foundation SDK` verification job. Land the exact passing commit before
publication. Publishing is separate from landing and requires maintainer
authorization.

For the first release of new crate names, an authorized maintainer can use
Cargo's existing local credential after candidate validation and landing:

```sh
bash scripts/foundation-sdk-packages.sh publish
```

Do not put that credential in repository files, shell arguments or logs. Once
each package has a Trusted Publisher configured for `LioRael/lenso-agent` and
`release-foundation-sdk.yml`, subsequent releases can use the dedicated workflow
on `main`, with its exact reviewed commit as `revision` and `publish: true`.
The workflow authenticates using OIDC and refuses a mismatched revision.

After publication, run `scripts/verify-foundation-sdk-registry.py`. It copies
the external durable-state App outside the checkout, pins all six SDK versions,
rejects non-registry Lenso dependencies, and runs the process-restart lifecycle
tests. Publication and that consumer result are separate release evidence.
