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

WORKDIR /app
COPY --from=builder /app/target/release/teddy-fyi-api-rust /app/

ENV PORT=8080
EXPOSE 8080

CMD ["./teddy-fyi-api-rust"]
