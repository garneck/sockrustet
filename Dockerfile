# Use the official Rust image for ARM64 as the builder
FROM --platform=linux/arm64 rust:1.85-slim as builder

WORKDIR /usr/src/app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy the Cargo.toml and Cargo.lock files
COPY Cargo.toml Cargo.lock ./

# Create a dummy main.rs to build dependencies
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Copy the actual source code
COPY src ./src

# Build the application for real
RUN touch src/main.rs && \
    cargo build --release

# Use Alpine for a small runtime image
FROM --platform=linux/arm64 alpine:3.18

# Install runtime dependencies
RUN apk --no-cache add ca-certificates

WORKDIR /app

# Copy the binary from the builder stage
COPY --from=builder /usr/src/app/target/release/sockrustet .

# Set environment variables
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
CMD ["./sockrustet"]

