# Request latency and provider concurrency

## Summary

All five requested changes are implemented. Independent QA: **PASS with documented performance tradeoffs**. Functional verification passed 24 distinct tests, including the official Claude CLI against a local fake API. Final buffered logging passed its two unit checks and existing API logging integration check again. Formatting, locked Clippy with warnings denied, and whitespace checks passed. Four final release benchmark runs passed. The full verdict is recorded in [qa-report.md](qa-report.md).

On the local workload with 100,000 historical requests, total p50 fell from roughly 101–115 ms to 9–22 ms. This is not a universal improvement: short responses without seeded history and large streaming responses became slower under the measured conditions. The complete comparison and retained per-run statistics are in [latency-measurements.md](latency-measurements.md).

## Requirements and implementation

| Requirement | Final behavior | Verification |
|---|---|---|
| Bounded routing usage reads | Typed account/provider totals replace request-history aggregation; a database trigger maintains both atomically | Populated migration, nullable usage, direct mutations, concurrent/duplicate finalization, rollback and isolation |
| Fewer credential/quota round trips | Fresh tokens use unlocked reads; refresh locks and rereads; quota snapshots use one ordered multirow statement | Held-lock inference, refresh races/cancellation, stale quota observations and atomic rollback |
| Dedicated log output | Bounded non-lossy library queue, buffered worker sink, explicit drain and retained sink errors | Queue pressure, slow output, sink failures, correlated full bodies/chunks and authentication-secret exclusion |
| Concurrent Claude inference | Host owns refresh tokens; independent CLI processes use access tokens and request coordinated refresh through the official control protocol | Concurrent subprocess barrier, one-connection pools, refresh deduplication, cancellation and official CLI 401 recovery |
| Accurate stage latency | Authentication, routing, credentials, provider setup and first nonempty text are measured; total includes authentication/routing | Controlled database/provider delays on both APIs, empty/tool-only/error cases and maintenance-stage separation |

The existing API → gateway → provider/store structure remains. PostgreSQL is authoritative across replicas. Refresh tasks persist rotation independently of caller cancellation. Historical statistics remain history-based; only routing reads use stored totals. The CLI credential snapshot/writeback path was removed because the host now owns refresh.

## Review outcomes

Architecture and implementation reviews resolved migration lock order, quota row-lock order, cancellation-safe refresh ownership, interrupted/error log writes, and logging test interference from global tracing state. Logging uses the library queue and standard buffer rather than a custom transport. An ordered flush acknowledgment is required because the library guard alone has a bounded shutdown wait. No unresolved correctness or security finding remains in the implementation review.

## Verification and measured results

- Full functional suite: 24 distinct tests passed; zero failures or ignored tests, with the manual benchmark filtered. Counts are logging unit 2, API 8, Claude 8, Codex 2, configuration 1, maintenance 1, gateway 2.
- Final buffering change: logging unit 2/2 and isolated API logging integration 1/1 passed again; the child-process invocation is not counted twice.
- Official unmodified Claude CLI 2.1.307: local inference, tools, quota collection and forced-401 OAuth control recovery passed.
- `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, and `git diff --check` passed.
- Final release comparison: baseline/final/final/baseline, both APIs and response modes, concurrency one/four, small/large payloads and 100,000-row history, with full JSON logging. All four runs passed.

The displayed changes below compare the mean of two per-run p50 values; they are not pooled percentiles.

| Local workload | Concurrency one | Concurrency four |
|---|---:|---:|
| Short response, 100,000 historical requests | 90.2–90.7% lower p50 | 81.1–81.7% lower p50 |
| Short response, no seeded history | 3.3–8.0% higher p50 | 20.7–28.4% higher p50 |
| Large streaming response, no seeded history | 15.2–17.4% higher p50 | About 28% higher p50 |

The regression rows are measured final behavior. Counter writes and logger queue transfer retain costs when the old history scan is cheap or the sink is a fast local file. The isolated routing SQL measurement on 120,000 total rows changed account usage from 24.339 ms to 0.047 ms and four-provider totals from 73.542 ms to 0.025 ms, using warm-buffer medians; these are database execution times, not API latency.

## Database and load impact

| Flow | Before | After |
|---|---|---|
| Routing totals | Account/provider queries aggregate request history | Same read-query count selects stored totals |
| Fresh credentials | Transaction and exclusive row-lock read | One unlocked read |
| Due credentials | Coordinated refresh transaction | An initial read followed by the coordinated lock/reread when needed |
| Quota snapshot with N windows | BEGIN + N INSERTs + COMMIT | One atomic INSERT; empty input does no I/O |
| Known usage mutation | Request row mutation | Same client statement plus two internal counter-row updates |
| Claude inference | Credential transaction held through subprocess lifetime | Short credential operations, independent subprocesses |

Each counter row gains two bigint fields; no new index is needed. Counter writes lock the account before its provider and can serialize completions for the same account. The migration locks request writers before parent-table changes, then backfills without discarding history. One 120,000-row migration took 129.5 ms including local invocation overhead; this does not predict a larger production migration.

The 256-record log queue applies backpressure instead of dropping records. Its capacity bounds record count rather than bytes. Formatting remains on the emitting thread; a stalled sink can delay requests and normal shutdown. The sink buffer has fixed bounded storage and is flushed by the library between batches and by the final drain.

## Artifacts

- Design and review: [PRD](prd.md), [architecture](architecture.md), [architecture review](architecture-review.md), [implementation review](implementation-review.md), [QA](qa-report.md).
- Evidence: [provider protocol research](provider-research.md), [database measurements](database-measurements.md), [release measurements](latency-measurements.md), [per-run statistics](latency-samples.csv).
- Runtime: usage migration, store, both providers, logging/main lifecycle, metrics, gateway and API timing.
- Validation: existing API/provider/gateway integration fixtures and focused log-writer checks.

## Limits

No real subscription call, live-service latency, client network transport, deployment log collector or high-load multi-replica benchmark was exercised. First text ends at the first nonempty gateway text delta; request total ends after durable accounting and excludes pre-handler body extraction and final client network drain. The local performance results include observable regressions and do not establish a uniform speedup.

The implementation preserves refresh persistence after local cancellation, but cannot make a remote OAuth exchange atomic with a later database commit or a lost network response. Revoked/absolutely expired credentials still require login. No unresolved product requirement remains; production capacity and migration duration depend on deployment workload.
