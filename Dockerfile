FROM rust:1.85-slim-bullseye AS builder

WORKDIR /usr/src/app

COPY Cargo.toml Cargo.lock ./

RUN cargo build --release

COPY src ./src

RUN cargo build --release

FROM debian:bullseye-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/target/release/sockrustet /usr/local/bin/sockrustet

ENTRYPOINT ["/usr/local/bin/sockrustet"]