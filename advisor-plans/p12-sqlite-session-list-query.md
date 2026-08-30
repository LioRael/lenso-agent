# P12 — SQLite Session-list query plan

Status: implemented and validated

## Outcome

Keep the durable correlated presentation query when a narrow partial index can
serve both projections; rewrite it only if the query plan cannot use that index.

## Work

- Capture `EXPLAIN QUERY PLAN` for the Session-list query.
- Add a narrow partial presentation index only if both correlated projections
  can use it without changing the durable data model.
- Avoid a latest-presentation table/join that would duplicate durable facts.
- Preserve ADR-0053 title and preview precedence exactly.

## Validation

- `EXPLAIN QUERY PLAN` produced eight steps and used the new partial presentation
  index twice, once for each latest-presentation correlated subquery.
- Existing title/preview precedence tests and all 11 SQLite Plugin tests passed.
- The five affected-package test command and all-target Clippy with warnings
  denied passed.
