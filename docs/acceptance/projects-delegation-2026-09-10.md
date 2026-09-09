# Projects delegated-user acceptance — 2026-09-10

## Scope and source

This was a local business App with PostgreSQL and actual Auth, Organization,
Access Control and Projects Plugins. It was not a deployed customer App.

- Auth browser integration: PR #101, merged as
  `397fa2f3cec6d66bab563a667b7f9ce8e6b11cbe` in `LioRael/lenso-auth-plugin`.
- Projects ingress and public Tool descriptions: PRs #6 and #8, with #8 merged as
  `569e58f5a8a7ea9beaad11004e13fb28c6f71e1c` in `LioRael/lenso-projects-plugin`.
- Agent business connection: PR #217 head
  `9c538d72f3b1fac47346556f396fea53d1653342` in `LioRael/lenso-agent`.
- Console browser consent: PR #329, merged as
  `a97f59b18a588c1656b45b507ea647e7f3bfa94a` in `LioRael/lenso-console`.

Registry dependencies included Organization PostgreSQL 0.5.0 and ACL PostgreSQL
0.2.0. Private Auth and Projects composition Plugins were built from source.

## Browser and actual model

Alice signed into the business App using its Password Plugin. Console initiated
browser consent; the App approved a narrowed child session and Console showed
connected state without receiving its credential. An actual model then read the
public Issue, updated only its title and read the result back.

Agent session: `9ab42ab6-a282-4546-9323-73b9c4ed077d`.
The final title was `Browser-authorized Agent acceptance passed`, revision `2`.
Independent PostgreSQL inspection confirmed:

- `issue_activity` recorded creation at revision 1 and update at revision 2 for
  the same signed-in user, `usr_Ej36CGMR4tVNd5hscy_ryrHd`.
- Description remained null, priority `medium`, workflow `started-public`,
  Project `project-public`, and Team `public`.

Full access was selected only for Agent tool approval in this isolated acceptance
instance. It did not bypass business authentication, operation audiences or ACL.

An earlier attempt failed because the parent/child audiences omitted the
intermediate Tool Provider execute hop. The final operation audience alone is
insufficient: Kernel filters authenticated context at every hop. The corrected
configuration and a negative final-operation test are included in the fixture.

## Protocol and lifecycle checks

The durable verifier was run twice against the same freshly prepared real App.
All 16 checks passed both times; the private Issue revision advanced from 1 to 2
and then 3. Each identical request replay returned the original result without a
second revision. Other checks covered consent origin and replay, public manifest,
anonymous and invalid-session denial, private-Team reads/writes, nonmember
Organization denial, stale revision rejection, absence of denied mutations,
operation narrowing, parent-session revocation and the other account remaining
usable. These HTTP protocol checks did not invoke a model.

The App exited cleanly on Ctrl-C. A database query confirmed that all five
schemas belonging to that run (`acceptance_13457_*`) had been removed.
Provider native tests separately cover Turn identity snapshots across account
switches, local disconnect, cancellation and missing/expired bindings.

## Reproduction and limits

Use [the acceptance guide](../../scripts/projects-acceptance/README.md).
The CI workflow pins Auth and Projects source commits and runs the protocol
scenario against PostgreSQL. CI results, packaged binary verification and npm
publication are separate receipts; this record alone does not prove them.

This acceptance does not claim a production business App deployment, simultaneous
multi-account model Turns, portable/Wasm business adapters or a distributed
transaction rollback on cancellation after a business write has completed.
