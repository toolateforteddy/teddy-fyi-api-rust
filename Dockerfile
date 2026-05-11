# Stage 1: Build the Rust application
FROM rust:1-bookworm AS builder
WORKDIR /usr/src/teddy-fyi-api-rust
COPY . .
# Build the release version for maximum performance
RUN cargo build --release

# Stage 2: Create a minimal runtime image
FROM debian:bookworm-slim
# Install certificates in case your API ever needs to make outgoing HTTPS requests
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app
# Copy the compiled binary from the builder stage
COPY --from=builder /usr/src/teddy-fyi-api-rust/target/release/teddy-fyi-api-rust /app/

ENV PORT=8080
EXPOSE 8080

CMD ["./teddy-fyi-api-rust"]
