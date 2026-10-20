# syntax=docker/dockerfile:1
FROM rust:1.99.1-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --locked --release

FROM debian:bookworm-slim AS claude
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl jq \
    && rm -rf /var/lib/apt/lists/*
ADD https://downloads.claude.ai/claude-code-releases/latest /claude-version
RUN set -eu; \
    case "$TARGETARCH" in amd64) platform=linux-x64 ;; arm64) platform=linux-arm64 ;; *) exit 1 ;; esac; \
    version="$(cat /claude-version)"; \
    base="https://downloads.claude.ai/claude-code-releases/$version"; \
    curl --fail --show-error --silent --location "$base/manifest.json" -o /manifest.json; \
    checksum="$(jq -er --arg platform "$platform" '.platforms[$platform].checksum' /manifest.json)"; \
    curl --fail --show-error --silent --location "$base/$platform/claude" -o /claude; \
    printf '%s  /claude\n' "$checksum" | sha256sum --check; \
    chmod 755 /claude

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libstdc++6 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/llmproxy /usr/local/bin/llmproxy
COPY --from=claude /claude /usr/local/bin/claude
RUN claude --version
ENV HOME=/tmp LLMPROXY_CONFIG=/etc/llmproxy/config.toml
USER 1000:1000
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/llmproxy"]
