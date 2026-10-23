# Management API architecture review

Date: 2026-10-23

## Result

PASS after architecture revision. The three initial findings below are resolved in the revised design and retained as implementation/verification obligations. The HTTP ownership boundary, separate browser session versus inference-token authentication, single-use signup-code transaction, shared per-kind backends, and two-query routing snapshot are cohesive and appropriately scoped. No global administration or additional identity service is needed.

Review scope: final PRD, architecture after its cookie-session correction, OAuth research, and the connected existing gateway, API, provider, quota, credential-lock and accounting code. This is a design/source review; no tests, live OAuth login, or cluster operation were run.

Revision recheck confirmed login-generation fencing, the one-row-per-provider session lock order, and reuse of periodic cleanup with `SKIP LOCKED`. These close the design gaps; runtime behavior still needs the specified integration tests.

## High: Re-login needs a credential generation boundary

The OAuth design allows replacing a provider's subscription credentials while an earlier inference continues. Existing quota rows are keyed only by provider ID/window, and `PgStore::save_quota` accepts later observations without knowing which login produced them. A fresh exhausted quota from account A therefore remains applicable after login to account B. Clearing quota at login alone is insufficient: a delayed A stream can write its quota again afterward.

The existing Claude callback has a related correctness problem. `ClaudeProvider::credentials(Some(rejected_token))` treats any newer stored access token as a usable refresh result. An old CLI invocation receiving a 401 after re-login could receive B's token and continue under a different upstream account. That violates the stated preservation of an admitted request's execution context.

**Correction accepted by the parent:** add a monotonically increasing `login_generation` to provider instances and quota rows. Increment it atomically only when new login credentials are committed; ordinary token refresh preserves it. Read the generation together with credentials in the existing credential SELECT. Carry it through the request, quota observations, and Claude refresh callbacks. Routing reads accept quota only when its generation matches the provider's current generation. The existing quota UPSERT must accept a newer generation, or a newer observation in the same generation; an old generation can never overwrite a newer generation. A Claude callback with a different generation fails the old request rather than injecting another login's token.

This requires no additional inference query, quota-write lock, provider factory, or history migration. It also avoids clearing a newer quota with a delayed old response.

**Validation:** extend the existing provider/OAuth concurrency fixture: hold an old stream, complete a new login, persist a new-generation quota, then release the old quota and a rejected-token callback. The new quota must remain authoritative, and the old callback must not receive the new login's token. A normal token refresh within one generation must still work.

## Medium: Expiry must not replace an actively exchanging login

The draft permits replacement of an expired `processing` attempt. An attempt can be claimed just before its browser deadline and remain inside its bounded HTTP exchange after that deadline. A new start then replaces it; the old exchange returns valid tokens but fails its final session-identity check, discarding them. HTTP cancellation tracking alone does not close this window.

**Correction:** keep one current login row per provider with `UNIQUE(provider_id)`. After the short durable processing claim, the tracked worker locks that session row for its actual exchange/save transaction and rechecks identity/status before network access. Replacement waits on the session row and rechecks after the active operation commits. Expired attempts left unlocked by a crashed process can still be replaced. A worker that loses the identity race before obtaining its lock makes no upstream call. Completion may commit after the browser deadline when the claim was valid and the operation still owns its row.

Avoid an explicit provider UPDATE lock in the start path. Once finalization updates `login_generation`, a start holding provider then waiting for session can deadlock a worker holding session then updating provider. Account-scoped session insertion/upsert plus the provider FK and immutable provider ownership/kind are sufficient. The worker's order is session row, existing credential row, bounded network operation, then provider generation/credential save/completion. It holds no account or provider-counter row during network I/O.

**Validation:** start completion immediately before expiry, block the fake token endpoint, advance beyond expiry, and start another login through a second app instance. Release the exchange and assert that successful credentials persist before replacement can proceed. Also prove an abandoned expired processing row can be replaced without replaying its code.

## Medium: Expired session storage needs a cleanup owner

Finite authentication validity does not remove database rows. Repeated ordinary login creates `management_sessions` indefinitely, and expired OAuth attempts can retain obsolete state/verifiers if no replacement happens. The architecture currently assigns expiry checks but no cleanup operation.

**Correction:** extend the existing periodic maintenance/reconciliation work with deletion of expired management sessions and expired OAuth attempts. Do not introduce another scheduler. For OAuth rows, use a short `FOR UPDATE SKIP LOCKED` selection so an actively exchanging worker is not removed or waited on. Keep the single current-row-per-provider design rather than inventing a login-history product. Index expiry where needed for the cleanup query.

**Validation:** the integration fixture should keep an active exchange locked while cleanup runs, remove ordinary expired rows, then prove active credential persistence still succeeds.

## Implementation conditions

- Keep all management mutations, including signup/login/logout, behind the fixed custom-header check; do not add permissive CORS later for the dashboard. The HttpOnly cookie should be host-only, with matching path/security attributes when cleared.
- Authentication input limits must precede expensive hashing. The bounded hash permit must remain owned by the blocking job after HTTP cancellation.
- Route replacement must use account-scoped SQL and one transaction. Strategy and routes must be read from the same statement snapshot as designed.
- Durable token latency derived from `requests.started_at/finished_at` currently measures admitted request execution, whereas the Prometheus duration starts at API-handler entry and includes authentication/routing/finalization. Name/document these boundaries; do not imply that their means are interchangeable.
- Keep the rejected-request counter separate from admitted lifecycle metrics. A later stream error belongs only to the existing gateway finalization owner.
- Update the OAuth research document's earlier account-bearer wording to the final management-cookie boundary so implementers do not preserve two authentication schemes.

## Load impact

The planned routing path uses two read statements instead of three and introduces no history aggregate on inference. Token-summary aggregation remains on explicit management reads. Password work is bounded and kept off async executor threads. Login network work retains only session/credential locks, not usage-counter locks. Per-token Prometheus labels deliberately increase series cardinality; the release comparison must include this added work and report mixed results rather than assert a universal improvement.

## SQL locking follow-up

An isolated PostgreSQL 17.11 probe confirmed that the normal fresh credential join must not be reused unchanged for locking credential reads. A writer updated a credential token from `old` to `new` and the joined provider generation from 0 to 1 within one transaction. While that transaction was uncommitted, another transaction ran `SELECT c.token,p.generation ... JOIN ... FOR UPDATE OF c`. After waiting for the writer, it returned `new|0`: a new locked credential with the old joined generation.

The same probe using `SELECT token FROM credentials ... FOR UPDATE` followed by a separate `SELECT generation FROM providers` returned `new` and `1`. Both assertions passed, and the temporary schema was removed. This matches PostgreSQL's documented [Read Committed locking-command behavior](https://www.postgresql.org/docs/17/transaction-iso.html#XACT-READ-COMMITTED), which can combine updated target rows with older rows elsewhere in the statement snapshot.

Use the two-statement sequence only on locked/refresh credential paths. Keep fresh request reads as the snapshot-consistent single JOIN. All new-login saves must acquire the credential row before updating the provider generation; that lock then keeps the separately read generation stable through refresh. No additional normal fresh-request query or duplicated generation column on the credential tables is needed. This was a SQL concurrency probe, not a Cargo or live OAuth test.
