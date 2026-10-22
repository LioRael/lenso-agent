# Delta checkout workflow

Tools: bun 1.4.2.


Delta invokes executable `prepare` at checkout root when mounting a new managed
checkout, before the agent and before cloud `setup`. Existing adopted or already
mounted checkouts may skip preparation. It performs fast checkout-local work only.
Executable `setup` installs tools/dependencies on Linux x86-64 before the first
turn; it is repeatable and a successful run is snapshotted. Reprovisioning can
recreate the runner, so neither cached dependencies nor running services are
assumed. Snapshot cache invalidation details are not specified by Delta docs.
Both scripts take no arguments. Exit 0 is success; nonzero reports failure.
This implementation uses 64 for invalid arguments and 78 for unmet prerequisites.
On macOS, setup exits without installation; run dependencies with existing tools.

No .env files, login state, linked files or host checkout paths are required or
copied. Do not run `gh auth setup-git`. No account app, secrets, deployment or
publication is part of setup. Keep service startup out of setup/snapshots.
Existing skills, review, CI and Land rules remain authoritative.

Sources: [Cloud Runners](https://delta.dev/docs/agents/cloud-runners),
[project setup](https://delta.dev/docs/configuration/project-setup),
[skills](https://delta.dev/docs/agents/skills),
[review and sync](https://delta.dev/docs/agents/review-and-sync).
