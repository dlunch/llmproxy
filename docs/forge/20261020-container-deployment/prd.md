# Container deployment and provider configuration

## Confirmed scope

- Remove compose.yaml and its setup instructions.
- Build and publish a multi-platform container to ghcr.io/dlunch/llmproxy with GitHub Actions: linux/amd64 on a native x86-64 runner, linux/arm64 on a native ARM64 runner, linux/arm/v7 cross-compiled on x86-64. No emulated compilation.
- Publish a combined image manifest on main pushes and release tags; validate pull requests without publishing. Use GITHUB_TOKEN for registry authentication.
- Provide a Helm chart deploying this image with external PostgreSQL credentials from an existing Secret, HTTP service/probes, configuration, and writable temporary storage. Preserve the current single-service ownership of provider credentials.
- Store Claude login/authentication configuration in PostgreSQL per provider instance. Restore only into a private temporary directory for CLI invocation and retain credential changes. Remove the old persistent config-directory option; no compatibility path.
- Store each provider's available model mappings in PostgreSQL, including client alias and upstream model name. Route using these rows instead of TOML model routes. Initial administration remains manual SQL.
- After implementation and verification, commit and push to origin/main. The repository currently has no commits, so the prior application implementation is included in the initial push.

## Container and runtime boundary

The upstream claude-universal project deliberately builds without redistributing the Claude executable in public images. Publish the proxy and system runtime dependencies; an operator supplies an architecture-compatible Claude executable through a volume. Authentication data lives in PostgreSQL, not that volume. Do not add a downloader/build service or administration endpoints.

## Acceptance

- Native amd64 and cross armv7 container builds are verified where available; the workflow uses an ARM64 hosted runner for arm64 and merges all three image digests.
- Helm lint/render checks verify configuration, Secret wiring, probes, replica/replacement strategy, and optional Claude executable volume.
- Integration tests prove provider/account-scoped model availability and retained model-based routing behavior.
- Claude tests exercise DB configuration hydration and persisted credential changes, including lifecycle termination paths and concurrent use where relevant.
- Existing API, gateway, and provider tests continue to pass. Report local checks separately from GitHub Actions and live-cluster validation.
- No credentials are committed or placed in image layers or ConfigMaps.
