# Stage 1: Cargo Chef Planner
FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Stage 2: Cargo Chef Builder
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - this layer will be cached in GHA cache
RUN cargo chef cook --release --recipe-path recipe.json

# Build the actual application
COPY . .
ENV SQLX_OFFLINE=true
RUN cargo build --release

# Stage 3: Minimal Runtime Image
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

# The service is a single self-contained binary that talks to Postgres, Redis and Google
# over the network and writes nothing to disk, so it has no reason to be root. A fixed
# uid/gid (not a distro-assigned one) keeps it stable across base image rebuilds and lets a
# Kubernetes securityContext pin `runAsUser: 10001` without reading the image first.
RUN groupadd --system --gid 10001 app \
    && useradd --system --uid 10001 --gid app --no-create-home --home-dir /app --shell /usr/sbin/nologin app

WORKDIR /app
# Owned by root, readable and executable by everyone: the binary is immutable at runtime,
# so the process user must not be able to rewrite its own executable.
COPY --from=builder --chown=root:root --chmod=755 /app/target/release/teddy-fyi-api-rust /app/

USER 10001:10001

ENV PORT=8080
# 8080 is above 1024, so dropping root costs nothing here — an unprivileged process can
# still bind it.
EXPOSE 8080

# No HEALTHCHECK. This image is only ever run by the GKE deployment, whose readiness and
# liveness probes are what actually gate traffic and restarts; Kubernetes ignores Docker's
# HEALTHCHECK entirely, so one here would be a second, silently-unused definition to keep
# in sync. The runtime image also has no curl or wget (adding one would put an HTTP client
# into the production image purely to check on itself), so a healthcheck would mean either
# a new package or a second binary entry point.

CMD ["./teddy-fyi-api-rust"]
