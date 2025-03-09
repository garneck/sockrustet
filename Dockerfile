# Build stage with optimized settings for scratch target
FROM rust:1.85-alpine AS builder

# Enable strict mode in shell commands with set -e to fail fast
SHELL ["/bin/sh", "-e", "-c"]

# Install build dependencies
RUN set -eo pipefail && \
    apk add --no-cache musl-dev openssl-dev pkgconfig build-base gcc 

# Configure Rust for static linking
RUN mkdir -p ~/.cargo
RUN echo '[target.x86_64-unknown-linux-musl]' >> ~/.cargo/config \
    && echo 'rustflags = ["-C", "target-feature=+crt-static"]' >> ~/.cargo/config

WORKDIR /app

# Copy source files
COPY . .

# Build with static linking
RUN set -eo pipefail && \
    cargo build --release --target x86_64-unknown-linux-musl

# Runtime stage using scratch (minimal image)
FROM scratch

WORKDIR /app

# Copy the built executable
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/sockrustet /app/

# Set environment variables for logging
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
ENTRYPOINT ["/app/sockrustet"]
