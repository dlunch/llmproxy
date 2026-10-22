# Database routing measurements

## Method

Measured PostgreSQL 17.11 in an isolated disposable schema. Applied unchanged `0001_initial.sql`, inserted 120,000 finalized request rows, then applied `0002_usage_totals.sql` in a transaction. The measured account owns 100,000 requests across four providers; a second account owns 20,000 across four other providers. Outcomes are 80% success, 10% failure, 5% cancellation, and 5% timeout. The last two outcomes have unknown usage. Known input counts vary from 200 through 8,199 and output counts from 50 through 1,049; timestamps span approximately fourteen days. The migration's resulting account totals matched the historical SUM result.

After ANALYZE, alternated the old account/provider SQL from base `2d13928` and the new SQL against the same unchanged migrated data. Executed each query nine times with `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`; discarded the first run and report medians of the remaining eight. Each explain used a fresh connection. Both plans returned one account row or four provider rows, respectively. Both used warm shared buffers with zero physical block reads in every retained run. Removed the isolated schema afterward.

Exact SQL, retained execution samples, planning medians, buffer counts, and representative complete JSON plans are in [database-measurements.json](database-measurements.json).

## Results

| Routing query | Historical aggregate median | Persisted total median | Historical shared hits | Persisted shared hits |
|---|---:|---:|---:|---:|
| Account usage | 24.339 ms | 0.047 ms | 2,667 | 1 |
| Four provider usage totals | 73.542 ms | 0.025 ms | 2,668 | 1 |

Retained execution ranges were 23.322–27.539 ms versus 0.044–0.056 ms for account usage, and 71.744–83.450 ms versus 0.021–0.031 ms for provider usage. These numbers are PostgreSQL execution time, excluding planning, connection setup, network time, and API work. Planning medians were 0.875/0.493 ms for old/new account reads and 1.903/1.052 ms for old/new provider reads; these are fresh EXPLAIN plans, not the application's prepared-statement behavior.

The account aggregate scanned all 120,000 request rows, retained 100,000, then aggregated. The provider aggregate scanned the same history, joined 100,000 rows to four providers, then grouped. New reads scanned only two account rows or eight provider rows, returning one account or four providers. PostgreSQL correctly chose sequential scans for these tiny parent tables. Query cost still scales with account/provider cardinality; it no longer scales with request history.

Migration/backfill wall time was **129.5 ms** for all 120,000 rows, including a fresh local Docker/psql invocation and connection setup. This is a single observation, not an isolated server-side migration benchmark. The migration holds a request writer lock during the backfill; larger histories and concurrent writers require separate operational assessment.

## Query/write tradeoff and limits

Routing retains one account query and one provider usage query; the existing quota query is unchanged. For each request mutation with a nonzero usage delta, the database trigger performs two additional row updates, account first and provider second, inside the original request statement's transaction. Null/zero deltas perform no counter writes. These are internal SQL writes, not extra client round trips. Finalizations for the same account can contend on its counter row. This read-focused measurement does not quantify that contention or write throughput.

For a quota write with N windows, client SQL statements fall from BEGIN + N INSERTs + COMMIT to one atomic multirow INSERT; zero windows issue no statement. Window-key ordering keeps concurrent writers' row-lock order consistent. No quota write timing claim is made here.

The host also ran other local development/fixture work; this was not an isolated hardware benchmark. The small sub-millisecond execution measurements are near instrumentation/noise limits. These results demonstrate elimination of the history scan on this dataset, not an equivalent end-to-end API speedup or a claim about real upstream inference latency. The release API comparison supplies the separate service-level evidence.
