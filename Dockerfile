FROM rust:1.85-slim

WORKDIR /usr/src/app

# Install build dependencies and debugging tools
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    file \
    procps \
    && rm -rf /var/lib/apt/lists/*

# Copy the entire project
COPY . .

# Build the application
RUN cargo build --release && \
    ls -la target/release/ && \
    file target/release/sockrustet

# Create app directory
RUN mkdir -p /app

# Copy and verify the binary
RUN cp target/release/sockrustet /app/ && \
    chmod +x /app/sockrustet && \
    ls -la /app && \
    file /app/sockrustet

# Set environment variables
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Use shell form to provide more context if it fails
CMD ["/bin/bash", "-c", "ls -la /app && file /app/sockrustet && exec /app/sockrustet"]
