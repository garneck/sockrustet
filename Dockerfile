# Use nightly Rust image
ARG TARGETPLATFORM

FROM rustlang/rust:nightly-slim AS builder

# Enable strict mode
SHELL ["/bin/sh", "-e", "-c"]

# Install required dependencies for proc-macros
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Optimize for build speed
ENV RUSTFLAGS="-C codegen-units=16 -C opt-level=1 -C target-feature=+crt-static"

WORKDIR /app

# Copy entire project
COPY . .

# Build without specifying target to use native architecture 
RUN echo "Building application..." && \
    rustc --version && \
    cargo --version && \
    cargo build --release && \
    cp target/release/sockrustet /app/sockrustet.bin

# Final stage with scratch
FROM scratch

WORKDIR /app

# Copy binary
COPY --from=builder /app/sockrustet.bin /app/sockrustet

# Set environment variables
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
ENTRYPOINT ["/app/sockrustet"]
