# Use nightly Rust image to support edition2024
FROM rustlang/rust:nightly-slim-bullseye AS builder

# Install cross-compilation tools for ARM64
RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc-aarch64-linux-gnu \
    libc6-dev-arm64-cross \
    && rm -rf /var/lib/apt/lists/*

# Set cross-compilation environment variables
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
ENV CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
ENV RUSTFLAGS="-C codegen-units=1 -C debuginfo=0 -C opt-level=3"
ENV CARGO_BUILD_JOBS=1

# Add the target for cross-compilation
RUN rustup target add aarch64-unknown-linux-gnu

WORKDIR /app

# Copy only the files needed to build dependencies
COPY Cargo.toml Cargo.lock ./

# Create a dummy project to cache dependencies
RUN mkdir -p src && \
    echo 'fn main() { println!("Dependency build"); }' > src/main.rs && \
    cargo build --target aarch64-unknown-linux-gnu --release && \
    rm -rf src/ target/aarch64-unknown-linux-gnu/release/deps/sockrustet*

# Copy actual source
COPY src/ src/

# Build the application
RUN echo "Starting final build..." && \
    cargo build --target aarch64-unknown-linux-gnu --release && \
    ls -la target/aarch64-unknown-linux-gnu/release && \
    cp target/aarch64-unknown-linux-gnu/release/sockrustet /app/sockrustet.bin

# Runtime stage
FROM scratch

WORKDIR /app

# Copy built binary
COPY --from=builder /app/sockrustet.bin /app/sockrustet

# Set environment variables
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
ENTRYPOINT ["/app/sockrustet"]
