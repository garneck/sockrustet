# Use nightly Rust image
ARG TARGETPLATFORM

FROM rustlang/rust:nightly-slim AS builder

# Enable strict mode
SHELL ["/bin/sh", "-e", "-c"]

# Optimize for build speed - use all CPU cores
ENV RUSTFLAGS="-C codegen-units=16 -C opt-level=1 -C target-feature=+crt-static"
# Use all available cores for faster builds (default value)
# Not setting CARGO_BUILD_JOBS will let cargo decide automatically

WORKDIR /app

# Copy entire project
COPY . .

# Build directly - no need for cross compilation
RUN echo "Building application..." && \
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
