# Use the official Rust image for ARM64 as the builder
FROM --platform=linux/arm64 rust:1.85-slim as builder

WORKDIR /usr/src/app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy the entire project
COPY . .

# Build the application directly and verify it exists
RUN cargo build --release && ls -la target/release/

# Use Alpine for a small runtime image
FROM --platform=linux/arm64 alpine:3.18

# Install runtime dependencies
RUN apk --no-cache add ca-certificates libgcc

WORKDIR /app

# Copy the binary from the builder stage and make it executable
COPY --from=builder /usr/src/app/target/release/sockrustet /app/
RUN chmod +x /app/sockrustet && ls -la /app

# Set environment variables
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary with explicit path
CMD ["/app/sockrustet"]

