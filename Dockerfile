# Use a standard Rust image without platform constraint
FROM rust:1.85-slim as builder

WORKDIR /usr/src/app

# Install build dependencies and debugging tools
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    file \
    && rm -rf /var/lib/apt/lists/*

# Copy the entire project
COPY . .

# Build the application directly with verbose output
RUN cargo build --release --verbose
RUN ls -la target/release/
RUN file target/release/sockrustet

# Use a standard Debian slim image for runtime
FROM debian:bullseye-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl1.1 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary from the builder stage and make it executable
COPY --from=builder /usr/src/app/target/release/sockrustet /app/
RUN chmod +x /app/sockrustet
RUN ls -la /app
RUN file /app/sockrustet || echo "Binary not found or not readable"

# Set environment variables
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary with more verbose error reporting
CMD ["/bin/sh", "-c", "ls -la /app && file /app/sockrustet && /app/sockrustet"]

