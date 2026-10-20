# Architecture review

Result: PASS. No blocking design findings.

The design satisfies idle refresh for both providers with one application maintenance loop and provider-owned OAuth exchanges. PostgreSQL row locking, eligibility and due checks inside the same transaction, and persistence on the same connection prevent concurrent replicas and inference from independently rotating one credential. Nonblocking maintenance locks avoid waiting behind Claude inference. Cancellation stops subsequent passes while draining an exchange already in flight.

Reusing the typed `updated_at` column is appropriate provided ordinary Claude profile/credential saves preserve it when token state is unchanged. The hourly unknown-expiry fallback and 24-hour refresh age remain effective while idle or under inference traffic. External Claude credential JSON remains document-shaped and unrelated metadata is preserved.

Implementation review must verify checked expiry conversions at the OAuth boundary, row locks restricted to credential rows, successful refresh freshness advancement, redacted errors, and lifecycle drain on both shutdown signals and server failure. Existing integration fixtures should cover observable token rotation and concurrency rather than duplicate internal implementation details.

## Persistence and load impact

Each configured provider performs at most one eligibility query per minute per replica. Fresh credentials release their transaction immediately; due credentials retain a single connection through the bounded OAuth call and commit. `SKIP LOCKED` prevents maintenance lock queues. No schema, indexes, history rows or credential duplication are introduced. The expected small personal provider registry makes this fixed cadence reasonable; there is no need for a separate scheduler or persisted jobs.

## Verification limits

This is a design review against `prd.md` and `architecture.md`. Runtime correctness and protocol response behavior require implementation review and local integration tests. Refresh cannot extend an upstream hard login lifetime or atomically recover an upstream rotation whose response or database commit is lost.
