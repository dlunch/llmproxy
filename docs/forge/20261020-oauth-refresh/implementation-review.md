# Implementation review

Result: PASS. No unresolved correctness, security, regression or scope findings.

Reviewed the complete runtime and test change against the preceding repository revision, including provider construction, shutdown ordering, credential readers/savers, shared scheduling and response handling, both provider refresh paths, and connected inference cancellation behavior.

## Correctness and concurrency

Maintenance starts after successful listener binding, runs immediately and every minute, and checks providers concurrently without overlapping passes. Shutdown signals stop scheduling and await the active pass; the server-error path also cancels and joins maintenance before draining provider workers.

Both maintenance readers filter enabled account ownership and use `FOR UPDATE OF c SKIP LOCKED`. Due checks use credentials loaded under that lock, and OAuth rotation is saved through its owned transaction. Codex inference shares the same refresh operation and retains its tracked task so request cancellation does not discard an in-flight rotation. Claude inference continues to hold the same credential row during official CLI execution and readback.

Claude token exchange uses the researched official endpoint, client ID and scopes, validates required access token and expiry, preserves optional omitted refresh fields and unrelated document/profile state, and bounds the response body. Errors and maintenance logs expose coarse classifications rather than token payloads. Claude persistence advances freshness only when OAuth token state changes, so ordinary inference cannot postpone unknown-expiry refresh.

## Scope and minimality

The change uses existing dependencies and transaction abstractions. The only added Cargo feature is Tokio test-clock support for development. No schema, production configuration, generic scheduler, compatibility path or billable refresh request was added. Existing Codex and Claude integration fixtures were extended; the lifecycle fixture covers scheduling behavior independently with virtual time.

One weak disabled-account test setup was corrected during review by making credentials due before disabling the account. Two test-only Clippy findings were corrected before final acceptance.

## Persistence and load impact

Each configured provider causes one indexed credential/ownership read per minute per replica. Fresh rows release promptly; due refreshes hold one connection through a bounded OAuth exchange and commit. Locked rows are skipped rather than queued. Updates replace credential state in place and do not add history or duplicate records. The design is appropriate for the stated small personal provider registry.

## Verification

The independent full Rust suite passed all 19 tests, including ignored PostgreSQL/local-endpoint tests and the official Claude CLI against a local fake API. Formatting, Clippy with warnings denied, and whitespace validation passed. Live upstream OAuth and real account lifetime behavior were not exercised; external rotation followed by a lost response or failed commit remains inherently non-atomic.
