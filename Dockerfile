# syntax=docker/dockerfile:1

# The frontend is exported at Rust build time by build.rs, so Node and Rust
# intentionally live in the same builder stage.
FROM node:20-bookworm AS builder

ARG RUST_VERSION=1.88.0
# Debian 12 container images use the DEB822 source file. This build-time
# setting can be overridden with --build-arg APT_MIRROR=<mirror-origin>.
ARG APT_MIRROR=https://mirrors.ustc.edu.cn
ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

RUN sed -i "s|http://deb.debian.org|${APT_MIRROR}|g" /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        git \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/rustup-init \
    && sh /tmp/rustup-init -y --profile minimal --default-toolchain "${RUST_VERSION}" \
    && rm /tmp/rustup-init

WORKDIR /app

# Keep dependency installation cacheable while source files change.
COPY frontend/package.json frontend/package-lock.json ./frontend/
RUN npm ci --include=dev --prefix frontend

COPY . .
RUN cargo build --release --locked \
    && cargo install sqlx-cli --version 0.8.6 --locked \
        --no-default-features --features mysql

FROM debian:bookworm-slim AS runtime

ARG APT_MIRROR=https://mirrors.ustc.edu.cn
ARG APT_BOOTSTRAP_MIRROR=http://mirrors.ustc.edu.cn

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
FROM runtime AS migrator

COPY --from=builder --chown=app:app /usr/local/cargo/bin/sqlx /usr/local/bin/sqlx
COPY --from=builder --chown=app:app /app/migrations /migrations

ENTRYPOINT ["sqlx", "migrate", "run", "--source", "/migrations"]
