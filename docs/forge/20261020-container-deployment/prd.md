# Container deployment and provider configuration

## Confirmed scope

- Publish a multi-platform proxy image to ghcr.io/dlunch/llmproxy through GitHub Actions for linux/amd64 and linux/arm64, compiling each on its matching native runner.
- Resolve and bundle the latest unmodified official Claude Code executable for each architecture at image build time. Verify its SHA-256 checksum against the official release manifest. Runtime startup requires neither executable installation nor build tooling.
- Publish a combined image manifest on main pushes and release tags; validate pull requests without publishing. Authenticate GHCR using GITHUB_TOKEN.
- Provide a Helm chart with configurable replicas, rolling updates, an existing PostgreSQL Secret, HTTP service/probes and writable temporary storage. No persistent executable volume or initialization container is needed.
- Serialize shared credentials across replicas using PostgreSQL row-lock transactions: Claude invocation and persistence, and Codex token refresh. Reads and writes within each critical section use the same transaction connection.
- Store Claude authentication in PostgreSQL per provider instance. Hydrate a private temporary invocation directory and retain credential changes. Remove persistent authentication-directory configuration.
- Store provider-specific client model aliases and upstream model mappings in PostgreSQL. Filter by authenticated account and registered providers before routing. Initial administration remains manual SQL.
- Remove obsolete Compose setup, architecture-specific cross-compilation, emulation, executable updater/build tooling, storage configuration and supporting tests.
- After implementation and verification, commit and push to origin/main.

## Acceptance

- Verify the official executable checksum and native amd64 image startup locally; native ARM64 compilation and combined publication are observed in the hosted workflow.
- Exercise the bundled official Claude executable against a fake local API and retain existing provider/API integration coverage. Keep live subscription claims separate.
- Helm lint/render checks cover Secret wiring, configurable rolling replicas, probes, writable temporary storage and the bundled executable path.
- Separate provider instances sharing one database credential prove cross-replica serialization without requiring a second pool connection while a transaction is held.
- Model routing observes database model changes and preserves account ownership isolation.
- No subscription credentials enter source control, image layers or ConfigMaps.
