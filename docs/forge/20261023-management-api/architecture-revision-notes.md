# Architecture review decisions

## H1: Relogin changes upstream account while old requests remain active — accepted

Existing quota keys and the Claude refresh callback identified only the provider instance. A new login could therefore retain quota from the old upstream account, let delayed old-request quota overwrite the new account's observations, or give a running CLI a token from a different login. Add a monotonically increasing login generation on provider and quota rows. Capture it with credential reads, increment only during successful new-login commit, filter/fence quota by generation, and reject a callback from a prior generation. This adds no fresh-credential inference query and no quota-write row lock. Refreshing the same login keeps its generation.

The reviewer reproduced a PostgreSQL lock-wait case where `JOIN provider ... FOR UPDATE OF credential` returned the new credential with an old joined generation. Fresh reads keep their single JOIN; locked paths now acquire the credential first and read generation in a separate statement while holding that lock. This adds one read only to the existing locking/refresh path and avoids mixing login identities.

## H2: Expiry/replacement can invalidate an active token exchange — accepted

A durable processing flag alone does not serialize near-expiry replacement with save. Use one current login row per provider. After claiming, the tracked worker locks/rechecks that row across actual bounded HTTP and credential save; replacement/cleanup then wait or skip it. Recheck the session ID after acquiring the lock before any upstream call. Do not hold account/provider row locks during network I/O, and do not reverse the session-to-provider lock order: login start uses only account-scoped INSERT ON CONFLICT on the session row. Reuse existing periodic maintenance for expired-session cleanup with SKIP LOCKED.

## User scope corrections — incorporated

The architecture now uses email/password management login with separate HttpOnly SameSite=Strict cookies and mutation-header CSRF protection. Inference tokens are separately issued after login and have per-token metrics. The API is completed and committed before the requested static dashboard, which will consume the same-origin endpoints without storing management credentials in localStorage.
