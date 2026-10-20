# Container deployment and provider configuration

Date: 2026-10-20

Added native amd64/arm64 and cross-compiled armv7 GHCR image publication and a Helm chart for external PostgreSQL. Removed Compose. Moved Claude authentication and provider model mappings into PostgreSQL, retaining per-instance isolation and lifecycle-safe credential persistence.

See the [requirements](../forge/20261020-container-deployment/prd.md), [architecture](../forge/20261020-container-deployment/architecture.md), [review](../forge/20261020-container-deployment/implementation-review.md), and [QA](../forge/20261020-container-deployment/qa-report.md) for exact scope and verification evidence.
