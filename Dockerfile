# syntax=docker/dockerfile:1
FROM --platform=$BUILDPLATFORM rust:1.99.1-bookworm AS build
ARG TARGETARCH
ARG TARGETVARIANT
WORKDIR /build
RUN if [ "$TARGETARCH/$TARGETVARIANT" = arm/v7 ]; then \
      apt-get update && apt-get install -y --no-install-recommends gcc-arm-linux-gnueabihf libc6-dev-armhf-cross \
      && rm -rf /var/lib/apt/lists/*; \
    fi
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN set -eu; \
    case "$TARGETARCH/$TARGETVARIANT" in \
      amd64/) target=x86_64-unknown-linux-gnu ;; \
      arm64/|arm64/v8) target=aarch64-unknown-linux-gnu ;; \
      arm/v7) target=armv7-unknown-linux-gnueabihf; \
        export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc; \
        export CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc ;; \
      *) exit 1 ;; \
    esac; \
    rustup target add "$target"; \
    cargo build --locked --release --target "$target"; \
    cp "target/$target/release/llmproxy" /llmproxy

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libatomic1 ripgrep \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /llmproxy /usr/local/bin/llmproxy
ENV HOME=/tmp LLMPROXY_CONFIG=/etc/llmproxy/config.toml
USER 1000:1000
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/llmproxy"]
