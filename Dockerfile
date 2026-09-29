# syntax=docker/dockerfile:1

ARG RUST_VERSION=1.88.0
FROM rust:${RUST_VERSION}-bookworm AS rust-toolchain

# The frontend is exported at Rust build time by build.rs, so Node and Rust
# intentionally live in the same builder stage.
FROM node:20-bookworm AS builder

# Keep Debian's default source; override with --build-arg APT_MIRROR=<mirror-origin>.
# Debian 12 container images use the DEB822 source file.
ARG APT_MIRROR=http://deb.debian.org
ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

# Both images use Debian Bookworm; reuse the preinstalled Rust toolchain.
COPY --from=rust-toolchain /usr/local/cargo /usr/local/cargo
COPY --from=rust-toolchain /usr/local/rustup /usr/local/rustup

RUN sed -i "s|http://deb.debian.org|${APT_MIRROR}|g" /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        git \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Keep dependency installation cacheable while source files change.
COPY frontend/package.json frontend/package-lock.json ./frontend/
RUN npm ci --include=dev --prefix frontend

COPY . .
RUN cargo build --release --locked \
    && cargo install sqlx-cli --version 0.8.6 --locked \
        --no-default-features --features mysql

FROM debian:bookworm-slim AS app-base

ARG APT_MIRROR=http://deb.debian.org
ARG APT_BOOTSTRAP_MIRROR=http://deb.debian.org

RUN sed -i "s|http://deb.debian.org|${APT_BOOTSTRAP_MIRROR}|g" /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && sed -i "s|${APT_BOOTSTRAP_MIRROR}|${APT_MIRROR}|g" /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home app

COPY --from=builder --chown=app:app /app/target/release/Bangumi-Recorder /usr/local/bin/bangumi-recorder

USER app
ENV LISTEN=0.0.0.0 \
    LISTEN_PORT=8080 \
    RUST_LOG=info

EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl --fail --silent http://127.0.0.1:8080/api/v2/version || exit 1

ENTRYPOINT ["bangumi-recorder"]

# Keep migration tooling out of the application image.  Compose builds this
# target for the one-shot migration service, which runs SQLx's normal CLI.
FROM app-base AS migrator

COPY --from=builder --chown=app:app /usr/local/cargo/bin/sqlx /usr/local/bin/sqlx
COPY --from=builder --chown=app:app /app/migrations /migrations

ENTRYPOINT ["sqlx", "migrate", "run", "--source", "/migrations"]

# The last stage is the default for builds without --target. Inherit the
# application entrypoint and filesystem without the migrator's additions.
FROM app-base AS runtime
