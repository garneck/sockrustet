# Use nightly Rust image to support edition2024
FROM rustlang/rust:nightly-bullseye-slim AS builder

# Install cross-compilation tools for ARM64
RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc-aarch64-linux-gnu \
    libc6-dev-arm64-cross \
    musl-tools \
    && rm -rf /var/lib/apt/lists/*

# Set cross-compilation environment variables for static linking
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc
ENV CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc
ENV RUSTFLAGS="-C target-feature=+crt-static -C codegen-units=1 -C debuginfo=0 -C opt-level=3"
ENV CARGO_BUILD_JOBS=1

# Add the MUSL target for fully static binaries
RUN rustup target add aarch64-unknown-linux-musl

WORKDIR /app

# Copy only the files needed to build dependencies
COPY Cargo.toml Cargo.lock ./

# Create a dummy project to cache dependencies
RUN mkdir -p src && \
    echo 'fn main() { println!("Dependency build"); }' > src/main.rs && \
    cargo build --target aarch64-unknown-linux-musl --release && \
    rm -rf src/ target/aarch64-unknown-linux-musl/release/deps/sockrustet*

# Copy actual source
COPY src/ src/

# Build the application with static linking
RUN echo "Starting final build..." && \
    cargo build --target aarch64-unknown-linux-musl --release && \
    ls -la target/aarch64-unknown-linux-musl/release && \
    cp target/aarch64-unknown-linux-musl/release/sockrustet /app/sockrustet.bin && \
    file /app/sockrustet.bin && \
    ls -la /app

# Use Alpine for the intermediate stage to verify the binary
FROM alpine:latest AS verify

WORKDIR /verify

# Copy the binary for verification
COPY --from=builder /app/sockrustet.bin /verify/sockrustet

# Verify the binary exists and is executable
RUN ls -la /verify && \
    chmod +x /verify/sockrustet && \
    file /verify/sockrustet

# Final stage with scratch
FROM scratch

WORKDIR /app

# Copy the verified binary
COPY --from=verify /verify/sockrustet /app/sockrustet

# Set environment variables
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
ENTRYPOINT ["/app/sockrustet"]
