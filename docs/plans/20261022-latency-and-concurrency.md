# Request latency and provider concurrency

- Date: 2026-10-22
- Status: implementation and local verification complete
- QA: [PASS with documented performance tradeoffs](../forge/20261022-latency-and-concurrency/qa-report.md)

The user accepted all five follow-up improvements after the initial copying/allocation optimization: bounded usage reads, fewer credential/quota round trips, worker logging, concurrent Claude inference, and stage metrics.

PostgreSQL now maintains typed account/provider token totals atomically with request mutations. Fresh credentials avoid exclusive locks, quota windows are written in one statement, and the host owns Claude refresh tokens while independent CLI processes use the official OAuth control callback. Logging uses a bounded non-lossy library worker with buffered output and an explicit drain. Fixed stage labels measure authentication through first actual text and durable completion.

The functional suite passed 24 distinct tests, including official CLI 2.1.307 local fixtures. Final buffered logging passed its focused checks again. Formatting, locked Clippy with warnings denied, and whitespace checks passed. Four final release benchmark runs compared the same fixture against `2d13928`, with full logs, both APIs, concurrency one/four, and 100,000 historical requests.

The 100,000-row workload reduced total p50 by approximately 81–91%. Small-history short responses and large streams regressed in the measured conditions; this is explicitly recorded rather than presented as a universal speedup. Migration blocks request writers during initialization, counter updates contend by account, and a saturated/stalled log sink can delay requests or shutdown. No live subscription or production capacity measurement was made.

See the [final report](../forge/20261022-latency-and-concurrency/final-report.md), [release measurements](../forge/20261022-latency-and-concurrency/latency-measurements.md), and [database evidence](../forge/20261022-latency-and-concurrency/database-measurements.md). Runtime changes cover the migration/store, providers, logger/main, metrics, gateway and API; existing integration fixtures cover the contracts. No product decision remains open.
