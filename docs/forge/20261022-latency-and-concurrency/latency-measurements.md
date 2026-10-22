# Release API latency measurements

## Method

Compared base `2d13928` with the final implementation using release binaries and the same benchmark fixture. The baseline writes full JSON logs directly to a temporary file; the final version uses its production bounded worker and buffered sink. Package release artifacts were cleaned before each variant build. The final measurement order was baseline, final, final, baseline, with no concurrent test or build work.

Each run exercises both API surfaces, streaming and collected responses, at concurrency one and four. Each combination uses five warm-up batches and 50 measured batches: 50 requests at concurrency one or 200 at concurrency four. Three separately created datasets cover a short response with no seeded history, 128 KiB input plus 2,048 synthetic 64-byte output deltas with no seeded history, and a short response after 100,000 finalized requests. Seeding and ANALYZE are outside request timings. Each dataset accumulates its own benchmark requests across combinations. PostgreSQL and request finalization, including counter updates, are included.

First text means a nonempty text delta parsed from the returned SSE body; initial metadata frames do not count. For collected responses, text becomes available when the complete response body arrives. Total latency ends after consuming the in-process response body. Full JSON logging is enabled, with the final drain outside per-request timing. The fixture uses a synthetic provider, four Tokio workers, and a local PostgreSQL 17.11 database; it excludes live provider/CLI latency, client network transport, and the deployment log collector.

Every displayed value is the arithmetic mean of the corresponding statistic from two runs, not a percentile pooled over individual requests. In particular, p95 at concurrency one uses only 50 observations per run and is noisy. Throughput uses all measured batches, including their slowest requests. Exact per-run statistics are retained in [latency-samples.csv](latency-samples.csv). All four runs passed their response-completion assertions.

## Results

### Short response, no seeded history

| API / mode | Concurrent | First text p50, ms | Total p50, ms | Total p95, ms | Requests/s |
|---|---:|---:|---:|---:|---:|
| Chat, collected | 1 | 9.25 → 9.68 | 9.25 → 9.68 | 12.37 → 11.44 | 103.10 → 100.80 |
| Chat, stream | 1 | 5.12 → 5.18 | 9.10 → 9.40 | 11.50 → 11.56 | 95.25 → 86.75 |
| Responses, collected | 1 | 8.85 → 9.44 | 8.85 → 9.44 | 10.89 → 10.73 | 110.45 → 103.95 |
| Responses, stream | 1 | 5.06 → 5.27 | 8.89 → 9.60 | 11.29 → 12.34 | 106.70 → 101.60 |
| Chat, collected | 4 | 16.31 → 19.95 | 16.31 → 19.95 | 19.03 → 26.36 | 232.90 → 129.00 |
| Chat, stream | 4 | 9.21 → 9.29 | 16.37 → 19.76 | 19.49 → 25.61 | 231.35 → 159.70 |
| Responses, collected | 4 | 16.57 → 20.25 | 16.57 → 20.25 | 19.15 → 26.42 | 229.60 → 155.45 |
| Responses, stream | 4 | 9.77 → 10.06 | 17.30 → 22.21 | 23.12 → 36.26 | 215.20 → 106.75 |

### Large payload, no seeded history

| API / mode | Concurrent | First text p50, ms | Total p50, ms | Total p95, ms | Requests/s |
|---|---:|---:|---:|---:|---:|
| Chat, collected | 1 | 16.14 → 14.04 | 16.14 → 14.04 | 20.65 → 15.69 | 61.85 → 70.55 |
| Chat, stream | 1 | 6.01 → 5.73 | 31.62 → 36.43 | 37.66 → 42.24 | 31.00 → 27.05 |
| Responses, collected | 1 | 14.14 → 13.69 | 14.14 → 13.70 | 16.10 → 15.51 | 69.90 → 72.75 |
| Responses, stream | 1 | 5.83 → 5.63 | 31.27 → 36.71 | 34.52 → 43.97 | 31.60 → 25.95 |
| Chat, collected | 4 | 23.71 → 24.43 | 23.71 → 24.43 | 29.88 → 50.27 | 146.45 → 137.40 |
| Chat, stream | 4 | 30.03 → 28.93 | 70.78 → 90.64 | 95.11 → 179.84 | 42.60 → 29.50 |
| Responses, collected | 4 | 23.54 → 23.24 | 23.54 → 23.24 | 29.22 → 34.31 | 151.80 → 148.55 |
| Responses, stream | 4 | 28.19 → 32.67 | 70.13 → 89.93 | 96.71 → 140.82 | 41.45 → 30.65 |

### Short response, 100,000 seeded requests

| API / mode | Concurrent | First text p50, ms | Total p50, ms | Total p95, ms | Requests/s |
|---|---:|---:|---:|---:|---:|
| Chat, collected | 1 | 102.87 → 9.89 | 102.87 → 9.89 | 108.81 → 12.40 | 9.70 → 98.45 |
| Chat, stream | 1 | 96.40 → 5.18 | 100.89 → 9.40 | 107.44 → 11.39 | 9.75 → 88.30 |
| Responses, collected | 1 | 100.74 → 9.89 | 100.74 → 9.89 | 106.10 → 13.34 | 9.80 → 97.55 |
| Responses, stream | 1 | 99.59 → 5.54 | 104.36 → 9.91 | 120.79 → 12.27 | 9.25 → 98.20 |
| Chat, collected | 4 | 114.88 → 21.60 | 114.88 → 21.60 | 159.47 → 34.47 | 30.45 → 139.50 |
| Chat, stream | 4 | 104.72 → 10.06 | 112.81 → 21.31 | 154.25 → 28.08 | 31.95 → 146.60 |
| Responses, collected | 4 | 112.66 → 21.04 | 112.66 → 21.04 | 146.44 → 28.28 | 32.60 → 147.00 |
| Responses, stream | 4 | 106.11 → 9.98 | 114.42 → 20.93 | 153.06 → 27.42 | 31.40 → 150.15 |

## Interpretation and limits

Persisted usage totals eliminate the request-history scan from routing. The 100,000-row API workload shows the resulting service-level improvement, separately from [isolated database query measurements](database-measurements.md). This does not imply an equivalent reduction in real model response time.

For 100,000 seeded requests, mean per-run total p50 fell by 90.2–90.7% at concurrency one and 81.1–81.7% at concurrency four. With no seeded history, short-response total p50 increased by 3.3–8.0% at concurrency one and 20.7–28.4% at concurrency four. Large-stream total p50 increased by 15.2–17.4% at concurrency one and about 28% at concurrency four. These regressions remain in the final implementation; this is not a uniform speedup across workloads.

Counter maintenance adds two short database row updates when reported usage changes. Small-history requests therefore have a write-cost tradeoff, and concurrent completions for the same account can contend. The measured concurrency-four cases exercise this contention but are not a high-load saturation test.

Worker logging does not remove JSON formatting or queue transfer costs. The sink uses a standard buffer to combine small writes; a bounded non-lossy queue still blocks emitters under saturation. An initial diagnostic implementation without this buffer showed a large-stream regression and was replaced before these final measurements. A fast local file is not representative of every deployment logging sink.

The host was not reserved exclusively for benchmarking, and PostgreSQL background activity was not disabled. Percentiles and throughput show run-to-run variation. Results describe these local workloads, not a universal latency improvement or a production capacity guarantee. Real subscription calls and multi-replica load were not measured.
