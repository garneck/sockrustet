# Use an official Rust runtime as a parent image (ARM64 native)
FROM rust:1.76-slim-bullseye AS builder

# Set working directory
WORKDIR /usr/src/app

# Copy the Cargo files first for better caching
COPY Cargo.toml Cargo.lock ./

# Create a dummy main.rs to cache dependencies
RUN mkdir src && echo "fn main() { println!(\"Hello, world!\"); }" > src/main.rs
RUN cargo build --release

# Copy the actual source code
COPY src ./src

# Build the application (native ARM64)
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