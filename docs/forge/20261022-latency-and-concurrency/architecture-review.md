# Architecture review

Result: PASS. Both required refinements below were implemented and verified in the final change. No broader redesign is required. Reviewed the PRD, architecture, current request/accounting/provider lifecycle, and official logging-worker source.

## Required refinements

### High: acquire the requests writer lock before altering parent tables

The usage migration must lock `requests` against writers before `ALTER TABLE accounts` or `provider_instances`, not only before taking the backfill snapshot. A concurrent request insert can otherwise hold its requests-table lock while waiting for a foreign-key parent lock held by the migration, while the migration waits for requests. Use the requests-first ordering for the entire migration transaction; then backfill and install the trigger atomically. This also ensures existing writers cannot escape initialization or get counted twice.

### Medium: retain earlier sink errors through the shutdown barrier

The logging sink wrapper must retain the first write/flush error and include it in the barrier acknowledgment. A later successful marker flush must not claim successful drain after an earlier JSON record was lost. The official worker writes each queued message independently but discards sink errors and continues; its own error counter does not cover these failures. Test a transient record-write failure followed by a successful flush. [Official worker source](https://raw.githubusercontent.com/tokio-rs/tracing/master/tracing-appender/src/worker.rs) confirms both preserved message boundaries and error swallowing.

## Accepted design

- PostgreSQL triggers atomically maintain account/provider totals with request mutations, preserve historical nullable usage, and retain the existing finalization gate. Runtime routing reads become independent of history size. Existing historical statistics remain available.
- Fresh credentials use unlocked reads. Due refreshes lock, reread and persist through tracked work; cancellation does not discard rotations. Claude children receive access tokens only, while the proven official SDK refresh control exchange preserves mid-session recovery. Comparing each child's rejected token against the locked authoritative token deduplicates forced refresh across replicas.
- Quota observations use one atomic statement with the existing timestamp guard. No added cache or schema is needed for quotas.
- The bounded library logging queue preserves complete JSON records under normal operation and applies backpressure when saturated. Explicit acknowledgment removes the library guard's finite drain-time limitation. Formatting remains on emitters, and queue capacity bounds records rather than bytes; performance claims must preserve these limits.
- Timing originates before authentication. Stage labels are finite; first-text counts only the first nonempty text event, while tool-only calls have no such observation. Total duration ends after durable accounting and includes authentication/routing. The documented exclusion of pre-handler body extraction and final client network drain is accurate.

## Persistence and load impact

Routing replaces growing history scans with fixed-row total reads. Each nonzero usage change adds two parent-row counter updates, serialized account then provider; account contention must be measured. The one-time migration scans existing history and blocks request writers. Fresh credential reads remove transaction/lock round trips; short refresh locks remain necessary. Multiwindow quota saves remove explicit transaction and per-window round trips while preserving atomicity. Independent Claude inference releases connections and credential locks before subprocess execution.

## Verification required for implementation acceptance

Use existing fixtures for historical migration/backfill, concurrent and duplicate finalization, rollback after counter failure, sparse quota ordering, credential races, cancelled refresh persistence, same-provider CLI overlap, and official 401 control refresh. Logging must prove saturation backpressure, JSON NUL escaping, slow-sink drain and earlier-write failure reporting. Controlled stage delays must distinguish actual text from metadata and include pre-gateway timing. Compare release measurements with base `2d13928` using the same fixtures and realistic history/logging; this design review makes no speedup claim.
