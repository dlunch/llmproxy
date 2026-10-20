# Implementation review

Status: accepted. No unresolved Critical, High, or Medium findings.

Reviewed Dockerfile, publication workflow, Helm templates, PostgreSQL model mappings, Claude credential storage and lifecycle, and their callers and integration tests. The chart now references the published `latest` tag and UID/GID 1000. Obsolete TOML routes, persistent Claude configuration directories, and Compose setup were removed.

The workflow isolates per-platform build artifacts to one run, publishes only on pushes, and merges exact digests after all builds succeed. Native amd64/arm64 runners and the BUILDPLATFORM Rust stage preserve native compilation; armv7 uses the cross linker. QEMU is limited to armv7 runtime package installation.

Model queries filter both account ownership and registered providers before policy selection. Credentials are hydrated inside a private temporary directory and only authentication fields are retained. Claude's per-provider lock covers load, invocation, process termination and persistence. Tracked workers handle receiver drop and cancellation; successful terminal events follow credential persistence. Function handoff also reaps the CLI before retaining rotated credentials.

## Persistence load

Model routing adds one account-scoped model query per inference. Each Claude invocation adds one credential read and one update, including failed or cancelled invocations after the process starts. Calls sharing a Claude provider serialize to protect refresh-token rotation; different provider instances remain independent. No request bodies or CLI transcripts are added to PostgreSQL.

## Verification limits

Integration tests validate application behavior and a real Claude executable against a fake local API. Live subscription authentication/rotation, hosted ARM64 execution, GHCR publication and Kubernetes rollout require their respective environments. See the QA report for completed commands and container build evidence.
