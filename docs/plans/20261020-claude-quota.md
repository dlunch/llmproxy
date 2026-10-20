# Claude response quota collection

## Purpose and decisions

Collect the five-hour and seven-day utilization/reset observations exposed by the bundled official Claude CLI in `rate_limit_event.rate_limit_info.unifiedWindows`. Preserve each observed window independently in the existing quota table. Missing measurements remain unknown; no separate usage API or estimated percentages are introduced.

Persist quota after committing credentials, before returning the terminal event. This preserves rotated tokens on quota-write failure and avoids requiring a second database connection while holding the credential lock.

The existing router applies its five-minute freshness policy and excludes exhausted included-usage windows even when upstream extra usage is allowed. Overage eligibility and model-specific quota routing are outside this collection change.

## Artifacts

- [Requirements](../forge/20261020-claude-quota/prd.md)
- [Architecture](../forge/20261020-claude-quota/architecture.md)
- [Final report](../forge/20261020-claude-quota/final-report.md)
