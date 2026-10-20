# Container deployment and provider configuration

Date: 2026-10-20

Added native amd64/arm64 GHCR image publication with the unmodified official Claude Code binary bundled at build time, plus a Helm chart for external PostgreSQL. Removed Compose. Moved Claude authentication and provider model mappings into PostgreSQL, retaining per-instance isolation and lifecycle-safe credential persistence. PostgreSQL row locks coordinate authentication across replicas; the chart supports rolling updates without executable volumes.

See the [requirements](../forge/20261020-container-deployment/prd.md), [architecture](../forge/20261020-container-deployment/architecture.md), [review](../forge/20261020-container-deployment/implementation-review.md), and [QA](../forge/20261020-container-deployment/qa-report.md) for exact scope and verification evidence.
