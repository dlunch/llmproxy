# Container deployment and provider configuration report

Implemented multi-platform container publication and Kubernetes deployment, together with the two database configuration requirements added during the task.

## Delivered

- Removed Compose and updated setup instructions for external PostgreSQL.
- Added a Dockerfile and GitHub Actions workflow for native amd64/arm64 builds and GNU cross-compilation for armv7. Only armv7 runtime package installation is emulated. Successful push builds publish exact platform digests and a combined GHCR image tagged by commit and main/version ref.
- Added a Helm chart with existing database Secret wiring, ConfigMap, ClusterIP Service, health probes, single-replica Recreate updates, and temporary storage. An optional read-only executable PVC and node selector support Claude without storing authentication on a volume.
- Moved available provider model mappings from TOML into PostgreSQL. Routing reads account-owned, registered providers and their model mappings on every request.
- Moved Claude login/authentication configuration into PostgreSQL. Each invocation hydrates a private temporary directory; a tracked worker serializes refresh, reaps the process and saves authentication changes before successful completion, including cancellation and tool handoff paths.

## Review and verification

No blocking implementation findings remain. The full application suite passed 17 tests, including the installed Claude executable against a local fake API. Final strengthened authentication lifecycle fixtures, formatting, Clippy, actionlint and Helm lint/render checks passed. Container and hosted publication results are recorded in [qa-report.md](qa-report.md).

## Persistence and operating constraints

Model lookup adds one scoped query per inference. Claude calls add one credentials read and one update after the process starts. Calls to the same Claude provider are serialized through authentication persistence; independent provider instances remain concurrent. The chart keeps one process per credential owner. Database history still excludes prompts, responses and transcripts.

The container includes distribution runtime libraries and ripgrep. Operators supply their architecture-compatible Claude executable, following the upstream project's build-only publication model. Real subscription calls and a live Kubernetes rollout were not performed; local rendering cannot establish operator-specific Secret/PVC permissions or cluster scheduling.
