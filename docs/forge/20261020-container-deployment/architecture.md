# Container deployment architecture

Build the existing Rust service as one OCI image for Linux amd64, arm64 and arm/v7. GitHub Actions distributes platform builds, then combines their pushed digests into one GHCR manifest. Kubernetes consumes that image through the Helm chart. PostgreSQL remains external.

## Container and build contract

The build stage uses `rust:1.99.1-bookworm` on `$BUILDPLATFORM`. amd64 and arm64 compile on their matching native Ubuntu runners. arm/v7 builds on amd64 with `gcc-arm-linux-gnueabihf` and `libc6-dev-armhf-cross`, the Rust `armv7-unknown-linux-gnueabihf` target, and the corresponding Cargo linker/CC configuration. Rust and C compilation always execute on the build platform. ARMv7 runtime package installation uses QEMU solely to run the distribution package manager; compilation is never emulated.

The runtime is `debian:bookworm-slim`, whose inspected package set already includes glibc, libgcc and libstdc++. Install ca-certificates, libatomic1 and ripgrep through the distribution package manager for the external Claude executable, and copy the compiled proxy. Run as UID/GID 1000 with `HOME=/tmp`, `LLMPROXY_CONFIG=/etc/llmproxy/config.toml`, and port 8080. PostgreSQL credentials enter through `DATABASE_URL`. A positive `.dockerignore` allowlist includes only Cargo manifests, source and migrations.

Claude is provisioned externally as an architecture-compatible executable. The upstream claude-universal workflow explicitly builds/tests without publishing its proprietary payload. The proxy image therefore contains only the proxy and distribution runtime dependencies. The Helm chart can mount the externally prepared executable; credentials and model routing follow the application's PostgreSQL contracts.

## Publication

`ubuntu-24.04` builds amd64 and cross-compiles arm/v7; `ubuntu-24.04-arm` builds arm64 natively. Pull requests build without registry login or publication. Main and version-tag pushes authenticate to GHCR using the repository token and `packages: write`, publish per-platform digests, then merge those exact digests. Main publishes `latest` and a commit tag; releases publish the version ref and commit tag. Concurrency is scoped to a workflow ref so newer changes supersede older builds without mixing artifacts from separate runs.

## Components and verification

| Component | Depends on | Verification |
|---|---|---|
| Container/runtime (core) | Rust source and migrations | Native image build; armv7 cross build and ELF machine inspection; native process startup |
| GitHub publication (core) | Container | Workflow/action validation; review PR/push conditions and digest merge; actual hosted run after authorized push |
| Helm deployment | Runtime contract | Helm lint/template, Secret/config/volume wiring, probes and security context |

The chart and image can be implemented in parallel after agreeing UID, config path, binary path and port. Final review checks that all three platform digests belong to the same workflow run and that armv7 never compiles under emulation. Native arm64 execution and GHCR publication are verified by the hosted workflow when available; local amd64 execution cannot substitute for either.

## Sources

- [Docker multi-platform builds](https://docs.docker.com/build/building/multi-platform/) documents native builders and `$BUILDPLATFORM` cross compilation.
- [Docker multi-platform GitHub Actions](https://docs.docker.com/build/ci/github-actions/multi-platform/) documents distributing platform builds and producing a combined image.
- [GitHub runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) lists native Ubuntu ARM64 runners.
- [Official Rust image](https://hub.docker.com/_/rust) lists Rust 1.99.1 Bookworm tags; the registry manifest was also inspected.
- [claude-universal workflow](https://github.com/dlunch/claude-universal/blob/main/.github/workflows/build.yml) defines its build-only publication boundary.

Project classification: backend deployment; no additional frontend/mobile guidance applies.

## Database-backed model availability

`provider_models(provider_id, model, upstream_model)` records client aliases and upstream names per provider instance, with a composite primary key and an instance foreign key. The gateway loads these routes on each request, scoped to the authenticated account and registered runtime provider IDs before calling any routing policy. Removing a model row changes availability on the next request without restarting the service. Runtime TOML registers provider implementations only; obsolete static route configuration is removed. Existing integration fixtures seed model rows and verify account isolation and runtime removal.
