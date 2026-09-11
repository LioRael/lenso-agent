# Projects delegated-user acceptance

This local App composes the actual Auth Account, Password, browser consent,
Organization, Access Control, Projects, Projects Tool and HTTP ingress Plugins
with PostgreSQL. Only the test caller, static test secrets and loopback HTTP
adapter are fixture mechanics. It does not mock business authorization or writes.

Use a dedicated disposable PostgreSQL database. The fixture uses public test
keys and passwords, binds only `127.0.0.1:55440`, and is not a production App.
It creates five process-specific schemas and removes those schemas on a clean
Ctrl-C shutdown. A crash deliberately leaves evidence for inspection.

## Run the protocol acceptance

Check out `LioRael/lenso-auth-plugin` containing PR #101 and
`LioRael/lenso-projects-plugin` containing PR #8. Their private composition
Plugins require these source checkouts; the Organization and ACL dependencies
come from crates.io. Paths are explicit and never discovered from another App.

```sh
python3 scripts/projects-acceptance/prepare.py \
  --auth-root ../lenso-auth-plugin --projects-root ../lenso-projects-plugin \
  --projects-web-root ../lenso-projects-web-plugin
export LENSO_POSTGRES_TEST_URL=postgresql://test_user@127.0.0.1:5432/test_database
export LENSO_ACCEPTANCE_RECEIPT="$PWD/.lenso/projects-acceptance/receipt.json"
cargo run --manifest-path .lenso/projects-acceptance/Cargo.toml
```

In another terminal, after the receipt exists:

```sh
python3 scripts/projects-acceptance/verify.py \
  --receipt .lenso/projects-acceptance/receipt.json
```

The verifier checks consent origin and replay, public descriptions without
execution authority, missing/invalid credentials, private Team and Organization
isolation, revision updates/conflicts, identical idempotent replay, denied writes
without mutation, final operation scope, parent revocation and account isolation.
It prints only non-secret evidence. HTTP protocol tests do not invoke a model.
Each run uses fresh idempotency keys and can run again against the same fixture.

## Exercise Console and a real model

1. Keep the App running. Enable the linked Business App connection in an isolated
   Agent Home by creating
   `plugins/lenso.agent.business-connection/projects.toml`:

   ```toml
   origin = "http://127.0.0.1:55440"
   label = "Projects acceptance"
   ```

2. Start Console against that Agent. Enable only the five delegated Projects
   Tools in its Tool policy. Model authentication is separate from business login.
3. Sign into the App at `http://127.0.0.1:55440/login` as `alice@example.test`
   with password `Local-acceptance-only-2026!`. In Console Connections, select
   Projects acceptance, sign in through the browser and approve its exact scope.
4. Read the non-secret organization ID from the receipt. Ask the model to read
   `issue-public`, change only its title, preserve all other aggregate fields and
   the observed `expected_revision`, then read it again. Verify the durable Issue
   and `issue_activity` actor, not only the model's final text.
5. Bob (`bob@example.test`, same test password) can access the public Team but
   cannot read or update `issue-private`. Alice cannot access the second
   Organization. Logging out through the App revokes the parent session and its
   delegated grant. Agent disconnect only drops local credentials; it is not a
   replacement for server-side revocation. Restart requires a new connection.

The protocol verifier logs Alice out. Reconnect Console after running it.
Browser and model acceptance must be recorded separately from protocol results.

## Audience configuration

Both the parent login session and narrowed child grant must include
`lenso.agent.tool-provider@2:execute` and each permitted final operation, such as
`lenso.projects@1:get_issue` and `lenso.projects@1:update_issue`. Auth context is
filtered at every Capability hop. The intermediate execute audience allows
forwarding; it does not grant an unlisted final operation. Include `:catalog`
only when using the authenticated catalog. The public manifest needs no grant.
Never solve an audience mismatch by copying signing material into Agent or by
accepting a model-supplied actor.

## Daily workflow browser acceptance

The fixture also links the standalone Projects Web Plugin. Pass its source
checkout with `--projects-web-root ../lenso-projects-web-plugin` when preparing
this App. This is required; the Web Plugin is not copied into Console.

After installing Console's locked browser test dependencies, run:

```sh
mkdir -p .lenso/browser-evidence
node scripts/projects-acceptance/browser.mjs \
  .lenso/projects-acceptance/receipt.json ../lenso-console .lenso/browser-evidence
```

The browser test uses only disposable fixture users. It verifies cookie login
and return to an Issue, login recovery from consent, explicit approval, workflow
update through the actual Tool ingress, conflict recovery and the refreshed
Issue/activity page. The grant stays inside the test process. It writes only a
non-secret receipt and screenshot. This test invokes the real business Tool
protocol, not a language model; model acceptance remains separate.

The current Issue contract has no assignee field. The fixture proves access to
visible Issues, not an assigned-to-me filter.

## Native Console Workspace

Prepare the App with the current Auth, Projects and Projects Web checkouts. Build
Projects Web and import `src/workspace` into Console with its
`scripts/import-projects-workspace.mjs`. Start the business App on 55440 and the
actual Console Host on 55450 with `LENSO_CONSOLE_PROJECTS_ORIGIN` pointing to 55440.
Use disposable Agent homes and the real local Agent binaries for Console bootstrap.

```sh
node scripts/projects-acceptance/console-browser.mjs RECEIPT CONSOLE_CHECKOUT OUTPUT
```

This test signs into the disposable business App, grants the Workspace connection,
reads Issue/activity/workflow data, follows native Console navigation without a
page reload, creates a project through the real business service, carries a bounded
Observe trace handoff into Projects, creates an Issue through business authorization,
and opens the existing mini agent with an unsubmitted Issue draft. It also checks
theme propagation, cross-organization denial, destination override rejection and
credential-free browser connection status. It resets only the dedicated acceptance
Console's Projects connection and creates test records. It does not call a model or
mutate production accounts.
