# Background OAuth refresh

Status: complete. Independent implementation review and QA passed: 19 tests passed with none ignored; formatting, Clippy with warnings denied, and whitespace checks passed.

## Delivered behavior

- Configured Codex and Claude providers check credentials at startup and every minute without inference requests.
- Refresh is due within five minutes of access-token expiry, after 24 hours without a token update, or hourly when expiry is unknown.
- Existing PostgreSQL locks serialize rotation with inference and other replicas. Missing, disabled and busy credentials are skipped; errors are redacted and retried on the next pass.
- Claude preserves external credential metadata and advances its timestamp only when token state changes. Shutdown drains ongoing refresh and persistence.

## Design and review

The implementation adds a provider maintenance hook and one application loop, reuses the bounded OAuth response reader, and adds Claude's official refresh-token protocol. It uses existing tables and runtime dependencies. Architecture review passed; final implementation review and QA results are recorded in their accompanying reports.

## Persistence impact

Each replica performs one credential-row lookup per configured provider per normal minute-long cycle. Due refreshes retain the credential row lock through the bounded OAuth call and save. Busy rows are skipped, and unchanged checks do not write credentials. No schema or index changes are required.

## Verification and limits

Provider integration tests cover idle rotation, replica coordination, freshness, retry, preservation of credentials, and existing inference. A virtual-clock lifecycle test covers startup, periodic execution, isolated failures, nonoverlap and shutdown drain. See [QA results](qa-report.md) for executed commands and counts.

Tests use synthetic credentials and local OAuth services; no real subscription refresh or billable inference was performed. Revocation and hard login expiry require reauthentication, and maintenance cannot run while the service is stopped. Upstream rotation and a database commit cannot be atomic across services.
