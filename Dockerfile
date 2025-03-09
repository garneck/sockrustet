# Use the latest Rust image with better proc-macro support
FROM rust:latest AS builder

# Enable strict mode
SHELL ["/bin/sh", "-e", "-c"]

# Install required dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Debug output to verify architecture
RUN uname -m && \
    rustc -vV && \
    rustup show

WORKDIR /app

# Copy entire project
COPY . .

# Build with explicit host detection
RUN echo "Building application..." && \
    HOST_ARCH=$(uname -m) && \
    echo "Detected architecture: $HOST_ARCH" && \
    if [ "$HOST_ARCH" = "aarch64" ] || [ "$HOST_ARCH" = "arm64" ]; then \
        echo "Building for ARM64" && \
        cargo build --release; \
    else \
        echo "Architecture not supported directly, using cross-compilation" && \
        rustup target add aarch64-unknown-linux-gnu && \
        cargo build --target aarch64-unknown-linux-gnu --release && \
        cp target/aarch64-unknown-linux-gnu/release/sockrustet target/release/ || true; \
    fi && \
    ls -la target/release/ && \
    cp target/release/sockrustet /app/sockrustet.bin || { echo "Binary not found!"; exit 1; }

# Final stage with distroless for better ARM64 compatibility
FROM gcr.io/distroless/static:nonroot

WORKDIR /app

# Copy binary
COPY --from=builder /app/sockrustet.bin /app/sockrustet

# Set environment variables
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
USER nonroot
ENTRYPOINT ["/app/sockrustet"]
