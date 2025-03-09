# Build stage with optimized settings for scratch target
FROM rust:1.85-alpine AS builder

# Enable strict mode in shell commands
SHELL ["/bin/sh", "-e", "-c"]

# Install build dependencies
RUN set -eo pipefail && \
    apk add --no-cache musl-dev openssl-dev pkgconfig build-base gcc

# Args for multi-architecture builds
ARG TARGETARCH=amd64

# Setup cross-compilation based on target architecture
RUN set -eo pipefail && \
    if [ "$TARGETARCH" = "arm64" ]; then \
        apk add --no-cache wget tar && \
        wget -qO- https://musl.cc/aarch64-linux-musl-cross.tgz | tar -xzC /opt/ && \
        export PATH="/opt/aarch64-linux-musl-cross/bin:${PATH}" && \
        mkdir -p ~/.cargo && \
        echo '[target.aarch64-unknown-linux-musl]' >> ~/.cargo/config && \
        echo 'linker = "aarch64-linux-musl-gcc"' >> ~/.cargo/config && \
        echo 'ar = "aarch64-linux-musl-ar"' >> ~/.cargo/config && \
        echo 'rustflags = ["-C", "target-feature=+crt-static"]' >> ~/.cargo/config && \
        rustup target add aarch64-unknown-linux-musl; \
    else \
        mkdir -p ~/.cargo && \
        echo '[target.x86_64-unknown-linux-musl]' >> ~/.cargo/config && \
        echo 'rustflags = ["-C", "target-feature=+crt-static"]' >> ~/.cargo/config; \
    fi

# Set environment variables for cross-compilation if needed
ENV CC_aarch64_unknown_linux_musl=aarch64-linux-musl-gcc
ENV CXX_aarch64_unknown_linux_musl=aarch64-linux-musl-g++
ENV AR_aarch64_unknown_linux_musl=aarch64-linux-musl-ar
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc

WORKDIR /app

# Copy source files
COPY . .

# Build with static linking for the appropriate target
RUN set -eo pipefail && \
    if [ "$TARGETARCH" = "arm64" ]; then \
        export PATH="/opt/aarch64-linux-musl-cross/bin:${PATH}" && \
        cargo build --release --target aarch64-unknown-linux-musl; \
    else \
        cargo build --release --target x86_64-unknown-linux-musl; \
    fi

# Runtime stage using scratch
FROM scratch

WORKDIR /app

# Args for multi-architecture builds
ARG TARGETARCH=amd64

# Copy the built executable based on architecture
COPY --from=builder /app/target/$([ "$TARGETARCH" = "arm64" ] && echo "aarch64" || echo "x86_64")-unknown-linux-musl/release/sockrustet /app/

# Set environment variables for logging
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
ENTRYPOINT ["/app/sockrustet"]
