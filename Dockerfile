FROM rust:1.85-slim

WORKDIR /usr/src/app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy the entire project
COPY . .

# Build the application
RUN cargo build --release

WORKDIR /app

# Copy the binary to the runtime directory
RUN cp /usr/src/app/target/release/sockrustet /app/ && \
    chmod +x /app/sockrustet

# Set environment variables
ENV RUST_LOG=info

# Expose the port
EXPOSE 3030

# Run the binary
CMD ["/app/sockrustet"]
