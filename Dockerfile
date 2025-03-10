# Use Rust nightly as the base image for ARM64
FROM rust:nightly-bullseye-slim AS builder

# Set working directory
WORKDIR /usr/src/app

# Install necessary dependencies (none needed beyond base image for now)
RUN apt-get update && apt-get install -y \
    && rm -rf /var/lib/apt/lists/*

# Set Rust toolchain to nightly explicitly
RUN rustup update && rustup default nightly

# Copy the Cargo files first for better caching
COPY Cargo.toml Cargo.lock ./

# Fetch dependencies without building a binary
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo fetch
# Clean up the dummy file to avoid confusion
RUN rm -rf src

# Copy the actual source code
COPY src ./src

# Build the application (native ARM64 with edition2024)
RUN cargo build --release

# Create runtime image
FROM debian:bullseye-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy the binary from builder
COPY --from=builder /usr/src/app/target/release/sockrustet /usr/local/bin/sockrustet

# Set the binary as the entrypoint
ENTRYPOINT ["/usr/local/bin/sockrustet"]