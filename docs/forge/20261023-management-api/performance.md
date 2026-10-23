# Management request-path measurements

Release benchmark against local PostgreSQL, both inference APIs, collected and streaming responses, concurrency 1/4, short requests, 128 KiB input with 2,048 output deltas, and 100,000 historical request rows. Each case uses five warm-up batches plus fifty measured batches (50 or 200 requests). Full JSON logging is enabled. Neither live provider latency nor browser/client network transport is included.

The baseline executable was built before these changes and retained; the after executable includes the new routing and per-token metrics. Two successful runs per version are recorded in `latency-samples.csv`. Execution order was before, after, after, before. The final baseline attempt returned 503 during a request and was excluded as incomplete; its retry completed. No remaining DB sessions/locks explained that failure. No production code was changed to mask it.

## Interpretation

The mean of the two per-run total-latency p50 values decreased in 14 of 24 cases and increased in 10. Changes span -24.5% to +97.3%. These are averages of reported medians, not pooled request percentiles. Results are too noisy to establish a general latency improvement or regression: the same after executable's first short Chat case measured 49.750 ms in one run and 8.406 ms in the other; the baseline measured 8.516 and 22.734 ms. All successful runs are retained rather than selecting the favorable one. The failed baseline also limits repeatability.

The structural load change is clear: account routing context and matching routes now use one statement, so routing reads decrease from three to two parallel reads. Per-token aggregate history queries run only in management. Normal fresh-credential inference remains one read; credential-lock paths add one generation read after acquiring the existing lock. Quota writes retain one batched statement. Background credential work is bounded to four jobs per replica. No optimization claim should be inferred from the noisy local timing matrix.

## Mean of two per-run total p50 measurements

| History | Large | API | Streaming | Concurrency | Before ms | After ms | Change |
|---:|---|---|---|---:|---:|---:|---:|
| 0 | false | completions | false | 1 | 15.625 | 29.078 | +86.1% |
| 0 | false | completions | true | 1 | 12.280 | 23.283 | +89.6% |
| 0 | false | responses | false | 1 | 12.198 | 17.817 | +46.1% |
| 0 | false | responses | true | 1 | 14.345 | 28.297 | +97.3% |
| 0 | false | completions | false | 4 | 22.546 | 39.540 | +75.4% |
| 0 | false | completions | true | 4 | 19.316 | 29.553 | +53.0% |
| 0 | false | responses | false | 4 | 19.874 | 23.090 | +16.2% |
| 0 | false | responses | true | 4 | 20.029 | 20.009 | -0.1% |
| 0 | true | completions | false | 1 | 12.028 | 12.108 | +0.7% |
| 0 | true | completions | true | 1 | 36.516 | 36.086 | -1.2% |
| 0 | true | responses | false | 1 | 12.262 | 12.145 | -1.0% |
| 0 | true | responses | true | 1 | 35.662 | 35.155 | -1.4% |
| 0 | true | completions | false | 4 | 19.963 | 21.949 | +9.9% |
| 0 | true | completions | true | 4 | 85.537 | 83.386 | -2.5% |
| 0 | true | responses | false | 4 | 22.038 | 18.831 | -14.6% |
| 0 | true | responses | true | 4 | 78.140 | 81.796 | +4.7% |
| 100000 | false | completions | false | 1 | 11.165 | 8.957 | -19.8% |
| 100000 | false | completions | true | 1 | 11.299 | 8.845 | -21.7% |
| 100000 | false | responses | false | 1 | 10.974 | 8.804 | -19.8% |
| 100000 | false | responses | true | 1 | 12.124 | 9.159 | -24.5% |
| 100000 | false | completions | false | 4 | 21.179 | 19.966 | -5.7% |
| 100000 | false | completions | true | 4 | 25.684 | 20.899 | -18.6% |
| 100000 | false | responses | false | 4 | 24.995 | 21.969 | -12.1% |
| 100000 | false | responses | true | 4 | 22.309 | 19.989 | -10.4% |
