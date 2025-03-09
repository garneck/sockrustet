# Build stage with optimized settings for arm64 target
FROM rust:1.85-alpine AS builder

# Enable strict mode in shell commands
SHELL ["/bin/sh", "-e", "-c"]

# Install build dependencies
RUN set -eo pipefail && \
    apk add --no-cache musl-dev openssl-dev pkgconfig build-base gcc wget tar

# Set target to arm64 only
RUN echo "aarch64-unknown-linux-musl" > /target.txt && \
    echo "Building for $(cat /target.txt)"

# Setup ARM64 cross-compilation
RUN TARGET=$(cat /target.txt) && \
    # Install ARM64 cross-compilation toolchain
    wget -qO- https://musl.cc/aarch64-linux-musl-cross.tgz | tar -xzC /opt/ && \
    # Configure Cargo for cross-compilation
    mkdir -p ~/.cargo && \
    echo "[target.$TARGET]" >> ~/.cargo/config && \
    echo "linker = \"aarch64-linux-musl-gcc\"" >> ~/.cargo/config && \
    echo "ar = \"aarch64-linux-musl-ar\"" >> ~/.cargo/config && \
    echo "rustflags = [\"-C\", \"target-feature=+crt-static\"]" >> ~/.cargo/config && \
    # Set environment variables for cross-compilation
    echo "export CC_aarch64_unknown_linux_musl=aarch64-linux-musl-gcc" >> /env.sh && \
    echo "export CXX_aarch64_unknown_linux_musl=aarch64-linux-musl-g++" >> /env.sh && \
    echo "export AR_aarch64_unknown_linux_musl=aarch64-linux-musl-ar" >> /env.sh && \
    echo "export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc" >> /env.sh && \
    echo "export PATH=/opt/aarch64-linux-musl-cross/bin:\$PATH" >> /env.sh && \
    # Add the rust target
    rustup target add $(cat /target.txt)

WORKDIR /app

# Create a dummy main.rs that doesn't need to compile our whole app
# but will still download our dependencies
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    echo 'fn main() { println!("Dummy build"); }' > src/main.rs

# Build dependencies only - this layer gets cached
RUN TARGET=$(cat /target.txt) && \
    source /env.sh && \
    cargo build --release --target $TARGET && \
    # Remove the built artifacts but keep the downloaded dependencies in cache
    cargo clean --release --target $TARGET --package sockrustet && \
    rm -rf src/

# Now copy the actual source code
COPY src/ src/

# Build with static linking for arm64 target
RUN TARGET=$(cat /target.txt) && \
    source /env.sh && \
    cargo build --release --target $TARGET && \
    # Copy the binary to a predictable location
    cp target/$(cat /target.txt)/release/sockrustet /app/sockrustet.bin

# Runtime stage using scratch
FROM scratch

WORKDIR /app

# Copy the built executable from the predictable location
COPY --from=builder /app/sockrustet.bin /app/sockrustet

# Set environment variables for logging
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
ENTRYPOINT ["/app/sockrustet"]
