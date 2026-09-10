# Projects daily workflow source acceptance

The local disposable App used real Auth Account, Password, browser consent,
Organization, Access Control, Projects, Agent Tools, and Projects Web Plugins
with PostgreSQL 18. No business authentication or mutation provider was mocked.

## Verified behavior

- An Issue link requires App login and returns to that same Issue after login.
- Unauthenticated consent offers App login, returns to consent, and requires
  explicit approval. The narrowed grant identifies the authenticated subject.
- A workflow catalog read followed by an exact-revision update changes the
  visible Issue to Done. Refresh shows the new revision and the real activity
  actor. A repeated stale revision returns actionable conflict guidance.
- The protocol suite passes 16 authorization/mutation checks, including private
  Team and Organization isolation, idempotency, no write on denial and parent
  revocation.
- Native connection tests verify private grants, immutable Turn identity,
  cancellation, shutdown, and that an old Turn's 401 cannot invalidate a newer
  connected account. The current rejected grant reports reconnect required.
- Console browser tests cover account presentation, reconnect, safe Issue links,
  and the existing activity disclosure. Unit tests only compute changes when a
  complete pre-read matches the mutation's expected revision.

## Evidence boundaries

The browser acceptance script invokes the actual business Tool protocol directly;
it does not invoke a language model. The previously published 1.11.0 model
acceptance is separate and is not reused as proof of these source changes.
No npm or binary release was performed for this iteration.

The source fixture pins merged Auth `b68d87654e18910e8d14649e48e3166636fa99a1`,
merged Projects `badf709faf5cb0e343498e3d15c264bb4e31a132`, and Projects Web
`556e3067509476199911f64bfd9e030ed09b4fad`. The Projects Web merge/release
state is separate from this local evidence.

The current Issue contract has no assignee field. This flow concerns visible
Issues supplied through their App links, not an assigned-to-me inbox.
Account presentation uses the App-issued subject ID; it does not infer a name
or email address. App owners still supply a real login route and session ingress.
