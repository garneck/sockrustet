# Build stage with optimized settings for scratch target
FROM rust:1.85-alpine AS builder

# Enable strict mode in shell commands
SHELL ["/bin/sh", "-e", "-c"]

# Args for multi-architecture builds
ARG TARGETPLATFORM
ARG BUILDPLATFORM

# Install build dependencies
RUN set -eo pipefail && \
    apk add --no-cache musl-dev openssl-dev pkgconfig build-base gcc

# Determine target architecture
RUN case "${TARGETPLATFORM}" in \
        "linux/arm64") echo "aarch64-unknown-linux-musl" > /target.txt ;; \
        *) echo "x86_64-unknown-linux-musl" > /target.txt ;; \
    esac && \
    echo "Building for $(cat /target.txt)"

# Setup cross-compilation based on target architecture
RUN TARGET=$(cat /target.txt) && \
    if [ "$TARGET" = "aarch64-unknown-linux-musl" ]; then \
        # Install ARM64 cross-compilation toolchain
        apk add --no-cache wget tar && \
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
        echo "export PATH=/opt/aarch64-linux-musl-cross/bin:\$PATH" >> /env.sh; \
    else \
        # Configure Cargo for x86_64
        mkdir -p ~/.cargo && \
        echo "[target.$TARGET]" >> ~/.cargo/config && \
        echo "rustflags = [\"-C\", \"target-feature=+crt-static\"]" >> ~/.cargo/config; \
    fi && \
    # Add the rust target
    rustup target add $(cat /target.txt)

WORKDIR /app

# Copy source files
COPY . .

# Build with static linking for the appropriate target
RUN TARGET=$(cat /target.txt) && \
    if [ -f /env.sh ]; then source /env.sh; fi && \
    cargo build --release --target $TARGET

# Runtime stage using scratch
FROM scratch

WORKDIR /app

# Copy the built executable based on architecture
COPY --from=builder /target.txt /target.txt
COPY --from=builder /app/target/$(cat /target.txt)/release/sockrustet /app/

# Set environment variables for logging
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
ENTRYPOINT ["/app/sockrustet"]
