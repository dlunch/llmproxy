# Container and Helm architecture

## Runtime image

One image contains the Rust proxy and the unmodified official Claude Code executable. Supported platforms are `linux/amd64` and `linux/arm64`, each built on a native GitHub runner. The multi-stage Dockerfile compiles Rust with the pinned Rust 1.99.1 Bookworm image, resolves the latest official Claude Code release on each image build and downloads it for the target platform, verifies its SHA-256 against the official release manifest, and copies both executables into Debian Bookworm slim. Download and build tools remain outside the runtime image. The final image runs `claude --version` during its build.

Claude is installed at `/usr/local/bin/claude`; model configuration refers to that stable executable. A remote `ADD` fetches the official latest-version pointer; changed contents invalidate the subsequent download layer. Updating Claude means rebuilding and deploying the resulting image. Startup requires no release download or executable installation. The runtime contains CA certificates and the C++ runtime needed by the official binary, runs as UID/GID 1000, and uses `/tmp` as its writable home. Runtime configuration is mounted at `/etc/llmproxy/config.toml`.

Anthropic explicitly permits customers to preinstall the unmodified Claude Code binary in their products under the Commercial Terms. Distribution permission and authentication permission are separate: Anthropic's documentation restricts third-party product use of subscription OAuth credentials and directs developers to API-key authentication. The deployment mechanism does not change application authentication behavior.

## Helm deployment

The chart deploys the same image directly as the application container, with a read-only configuration ConfigMap mount, Service, resource settings and HTTP readiness/liveness probes. Database connection settings come from a separate Secret. Database credentials and configured authentication remain in Secrets and PostgreSQL. Replica count is configurable; PostgreSQL row locks serialize provider authentication updates across replicas. An ephemeral writable `/tmp` volume provides the application home and invocation directories. No executable PVC or init container is needed because the image contains Claude.

## Publication

The image workflow builds amd64 on `ubuntu-24.04` and arm64 on `ubuntu-24.04-arm`. Pull requests build without registry login or publication. Main and version-tag pushes authenticate to GHCR using the repository token and `packages: write`, publish per-platform digests, then merge those exact digests. Main publishes `latest` and a commit tag; releases publish the version ref and commit tag. Workflow concurrency is scoped to a Git ref.

## Verification

Native image validation checks the official checksum and executable version, proxy startup and shutdown, API authentication and model discovery, request logging, stats and metrics. The existing real-CLI integration fixture exercises the bundled official Claude against a local API fixture. Helm lint and rendered-manifest checks validate configuration, probes, security settings and replicas. The hosted workflow verifies the native ARM64 image and final multi-platform registry manifest; local amd64 execution does not substitute for either.

## Database-backed model availability

`provider_models(provider_id, model, upstream_model)` records client aliases and upstream names per provider instance, with a composite primary key and an instance foreign key. The gateway loads these routes on each request, scoped to the authenticated account and registered runtime provider IDs before calling any routing policy. Removing a model row changes availability on the next request without restarting the service. Runtime TOML registers provider implementations only. Integration fixtures seed model rows and verify account isolation and runtime removal.

## Sources

- [Claude Code legal and compliance](https://code.claude.com/docs/en/legal-and-compliance#can-customers-offer-claude-code-in-their-products): permission to preinstall the unmodified binary and separate authentication restrictions.
- [Claude Code setup](https://code.claude.com/docs/en/setup): official native installation and supported platforms.
- [Docker multi-platform GitHub Actions](https://docs.docker.com/build/ci/github-actions/multi-platform/): native distributed builds and digest-based manifest publication.
- [GitHub runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners): native Ubuntu ARM64 runners.

Project classification: backend deployment; no additional frontend/mobile guidance applies.
