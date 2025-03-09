# Build stage with cargo-chef for optimization
FROM rust:1.85-alpine AS builder

# Install build dependencies including cross-compilation tools
RUN apk add --no-cache musl-dev openssl-dev pkgconfig build-base gcc 

# Install cross-compilation toolchain
RUN apk add --no-cache wget tar
RUN wget -qO- https://musl.cc/aarch64-linux-musl-cross.tgz | tar -xzC /opt/
ENV PATH="/opt/aarch64-linux-musl-cross/bin:${PATH}"

# Configure Rust to use the cross compiler for ARM64
RUN mkdir -p ~/.cargo
RUN echo '[target.aarch64-unknown-linux-musl]' >> ~/.cargo/config \
    && echo 'linker = "aarch64-linux-musl-gcc"' >> ~/.cargo/config \
    && echo 'ar = "aarch64-linux-musl-ar"' >> ~/.cargo/config \
    && echo 'rustflags = ["-C", "target-feature=+crt-static"]' >> ~/.cargo/config

# Set environment variables for cross-compilation
ENV CC_aarch64_unknown_linux_musl=aarch64-linux-musl-gcc
ENV CXX_aarch64_unknown_linux_musl=aarch64-linux-musl-g++
ENV AR_aarch64_unknown_linux_musl=aarch64-linux-musl-ar
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc

WORKDIR /app

# Copy source files
COPY . .

# Build for ARM64
RUN rustup target add aarch64-unknown-linux-musl && \
    cargo build --release --target aarch64-unknown-linux-musl

# Runtime stage using Alpine
FROM alpine:3.21

# Install minimal runtime dependencies
RUN apk add --no-cache ca-certificates tzdata

WORKDIR /app

# Copy the built executable
COPY --from=builder /app/target/aarch64-unknown-linux-musl/release/sockrustet /app/

# Set environment variables for logging
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
CMD ["/app/sockrustet"]
