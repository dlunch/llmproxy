# Claude quota collection

Status: complete. Independent review and QA passed: all 19 tests passed with none ignored, and formatting, Clippy with warnings denied, and whitespace checks passed.

The Claude provider now collects explicit five-hour and seven-day observations from the bundled official CLI's `rate_limit_event.rate_limit_info.unifiedWindows`. Each window stores its remaining fraction, reset time and observation time in the existing quota table; utilization at or above 100% maps to zero remaining. Missing or malformed observations do not invent percentages or overwrite valid data.

Collection extends the existing stream parser and finalization path. Authentication is committed before quota persistence, preserving rotated credentials even if the quota write fails and supporting one-connection pools. Already observed quotas survive request failure, cancellation and function handoff. There are no new dependencies, schema changes or usage API polling.

The existing routing policy now sees Claude's collected base windows: it excludes exhausted fresh windows, ignores observations after five minutes or reset, and does not account for extra-usage entitlement. This is conservative even when Claude permits paid overage.

Independent review and verification are recorded in [implementation review](implementation-review.md) and [QA report](qa-report.md). Existing integration fixtures are extended rather than adding duplicate tests. The feature adds at most one quota transaction containing two upserts per completed inference with observed quota windows; inference without window data adds no quota write.

The unmodified official Claude 2.1.304 executable was exercised against a local Messages API using a synthetic OAuth token. Header utilization values of 0.25 and 0.4 reached the database as remaining fractions 0.75 and 0.6 through actual CLI events. No real subscription credentials or billable upstream calls were used.
