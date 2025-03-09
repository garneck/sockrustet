# Platform-specific image for ARM64 builds
FROM --platform=linux/arm64 rustembedded/cross:aarch64-unknown-linux-musl AS builder

# Enable strict mode in shell commands
SHELL ["/bin/sh", "-e", "-c"]

# Reduce debug info to speed up compilation
ENV CARGO_PROFILE_RELEASE_DEBUG=0
ENV RUSTFLAGS="-C codegen-units=1 -C debuginfo=0 -C opt-level=3 -C target-feature=+crt-static"
ENV CARGO_BUILD_JOBS=1

WORKDIR /app

# Copy only the files needed to build dependencies
COPY Cargo.toml Cargo.lock ./

# Create a dummy project to build dependencies
RUN mkdir -p src && \
    echo 'fn main() { println!("Dependency build"); }' > src/main.rs && \
    cargo build --target aarch64-unknown-linux-musl --release && \
    rm -rf src/ target/aarch64-unknown-linux-musl/release/deps/sockrustet*

# Copy actual source
COPY src/ src/

# Optimize the final build - use LTO thin for faster linking but still good optimization
RUN echo "Starting final build..." && \
    RUSTFLAGS="$RUSTFLAGS -C lto=thin" \
    cargo build --target aarch64-unknown-linux-musl --release && \
    ls -la target/aarch64-unknown-linux-musl/release && \
    cp target/aarch64-unknown-linux-musl/release/sockrustet /app/sockrustet.bin

# Runtime stage - using scratch for minimal size
FROM scratch

WORKDIR /app

# Copy built binary
COPY --from=builder /app/sockrustet.bin /app/sockrustet

# Set environment variables for logging
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
ENTRYPOINT ["/app/sockrustet"]
