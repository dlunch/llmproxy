# Request-path latency optimization

## Changes

- Make minimizing request latency a durable constraint in `AGENTS.md`.
- Deserialize request types from the existing JSON tree without first cloning that tree; preserve request-body logging and validation.
- Build Responses delta events only when Responses streaming consumes them. Chat streaming no longer accumulates text, citations or search state that it never returns; function identity validation remains shared.
- Move owned JSON arrays and values into envelopes instead of recursively copying them. Move provider text deltas out of parsed JSON; serialize Claude transcript records directly into their final byte buffer.
- Compute the immutable configured-provider ID list once. Move selected route fields, borrow quota lookup IDs, and select the least-used provider without a candidate allocation.
- Run independent routing-state, route and account-usage reads concurrently through the existing pool. Queries, account scope, persistence and cancellation behavior are preserved.

## Measurement method

Baseline: commit `50a4d90`. Both versions used the same benchmark fixture, release builds and a local PostgreSQL instance. Build processes and functional tests finished before timing. Run order was before, after, after, before; each run used fresh database schemas.

Each API/mode/concurrency scenario uses 128 KiB of request text and 2,048 synthetic 64-byte output deltas. After five warm-up batches, the benchmark records 100 batches: 100 requests at concurrency one or 400 requests at concurrency four. The first-frame timer includes handler execution and reading the first body frame; the total timer ends after the response body is consumed. These are in-process fake-provider measurements, not live model, client-network or production logging-sink measurements.

The values below are arithmetic means of two per-run percentiles, in milliseconds; they are not percentiles of pooled samples.

| API | Streaming | Concurrency | First-frame p50 before → after | Total p50 before → after | Total p95 before → after |
|---|---|---:|---:|---:|---:|
| Chat | false | 1 | 11.912 → 10.743 | 11.941 → 10.745 | 13.662 → 12.809 |
| Chat | true | 1 | 5.625 → 5.364 | 16.218 → 15.412 | 17.800 → 17.770 |
| Responses | false | 1 | 11.697 → 10.264 | 11.700 → 10.265 | 14.152 → 11.934 |
| Responses | true | 1 | 5.727 → 5.413 | 16.956 → 16.291 | 19.633 → 18.203 |
| Chat | false | 4 | 25.385 → 17.017 | 25.385 → 17.017 | 41.597 → 22.380 |
| Chat | true | 4 | 10.223 → 9.901 | 37.709 → 33.398 | 42.565 → 38.013 |
| Responses | false | 4 | 22.474 → 17.600 | 22.475 → 17.601 | 26.584 → 21.438 |
| Responses | true | 4 | 10.770 → 9.918 | 37.353 → 36.201 | 44.934 → 40.912 |

The strongest improvement was nonstreaming at concurrency four. Streaming total-time differences were smaller and variable. First-frame tail latency did not consistently improve: Chat streaming at concurrency four changed from 11.683 ms to 12.108 ms for the mean per-run p95. No universal first-token or high-load latency claim is made. Concurrent reads can use more pool connections at once; only concurrency one and four were benchmarked.

Reproduce with a disposable `TEST_DATABASE_URL`:

```sh
cargo test --release --locked --test api_integration proxy_latency_benchmark -- --ignored --nocapture
```

## Verification

- All 19 functional tests passed with PostgreSQL and official Claude CLI 2.1.307 against a local fake API; the manual performance test was excluded from this functional run.
- The existing function-call roundtrip test now also verifies that changed call identity fails for both APIs in streaming and nonstreaming modes.
- The latency benchmark passed in all four runs.
- `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, and `git diff --check` passed.
- No runtime dependencies, schemas, authentication caches or logging-disable options were introduced. Required shared-handle clones remain where asynchronous ownership requires them.
