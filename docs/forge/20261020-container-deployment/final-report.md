# Container deployment and provider configuration report

Implemented multi-platform container publication and Kubernetes deployment, together with the two database configuration requirements added during the task.

## Delivered

- Removed Compose and updated setup instructions for external PostgreSQL.
- Added a Dockerfile and GitHub Actions workflow for native amd64/arm64 builds. The image bundles the unmodified official Claude Code binary, resolved from the latest release on each build and verified against its official checksum manifest. Successful push builds publish exact platform digests and a combined GHCR image tagged by commit and main/version ref.
- Added a Helm chart with existing database Secret wiring, ConfigMap, ClusterIP Service, health probes, rolling updates, and temporary storage. No executable PVC or startup installation is needed.
- Moved available provider model mappings from TOML into PostgreSQL. Routing reads account-owned, registered providers and their model mappings on every request.
- Moved Claude login/authentication configuration into PostgreSQL. Each invocation hydrates a private temporary directory; a tracked worker serializes refresh, reaps the process and saves authentication changes before successful completion, including cancellation and tool handoff paths. PostgreSQL row locks coordinate Claude invocations and Codex token refresh across replicas.

## Review and verification

No blocking implementation findings remain. The full application suite passed 17 tests, including the installed Claude executable against a local fake API. Final strengthened authentication lifecycle fixtures, formatting, Clippy, actionlint and Helm lint/render checks passed. The latest-aware native image built successfully, verified the official Claude 2.1.304 checksum, and passed non-root proxy startup, database routing, statistics, logging and shutdown checks. The unmodified official Claude binary also passed the parallel function round-trip fixture against a fake local API. Detailed results and hosted-publication status are recorded in [qa-report.md](qa-report.md).

## Persistence and operating constraints

Model lookup adds one scoped query per inference. Claude calls add one credentials read and one update after the process starts. Calls to the same Claude provider are serialized through authentication persistence; independent provider instances remain concurrent. Replicas use the same provider ID for a shared credential. Locks and credential writes use the same transaction connection. Database history still excludes prompts, responses and transcripts.

The container bundles the official Claude executable under Anthropic's documented preinstallation conditions. The intended deployment is personal and self-hosted. Real subscription calls and a live Kubernetes rollout were not performed; local rendering cannot establish operator-specific Secret permissions or cluster scheduling.
