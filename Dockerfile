FROM rust:1.85-slim-bullseye

WORKDIR /usr/src/app

COPY Cargo.toml Cargo.lock ./

COPY src ./src

RUN cargo build --release && \
    cp target/release/sockrustet /usr/local/bin/ && \
    chmod +x /usr/local/bin/sockrustetor use direct path to binary


ENTRYPOINT ["/usr/local/bin/sockrustet"]