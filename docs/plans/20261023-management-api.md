# Account management API

- Date: 2026-10-23
- Status: Implemented and tested

Email/password signup uses a database-issued single-use code; management sessions are separate from multiple inference API tokens. Account-scoped APIs register and authenticate subscription providers, edit labels/enablement, replace routing atomically, and return per-token metrics. Providers are now database-owned and visible across replicas without restart. Direct Codex OAuth and official Claude browser OAuth persist cancellation-safe credentials with relogin generation isolation.

The implementation keeps two parallel routing reads, normal single-read fresh credentials, batched quota writes, and four bounded background refresh jobs. Existing operator-managed IP authentication remains unchanged. The static dashboard follows in a separate commit.

- [Requirements](../forge/20261023-management-api/prd.md)
- [Architecture and review](../forge/20261023-management-api/architecture.md)
- [Delivery and limitations](../forge/20261023-management-api/final-report.md)
- [QA](../forge/20261023-management-api/qa-report.md)
- [Noisy local release measurements](../forge/20261023-management-api/performance.md)

Full verification includes 28 unique Rust tests, PostgreSQL/HTTP fixtures, and the official Claude CLI against a local mock API. No real account login or cluster rollout was performed. Runtime data/configuration cutover prerequisites are in README.
