FROM rust:1.85-slim-bullseye

WORKDIR /usr/src/app

COPY Cargo.toml Cargo.lock ./

COPY src ./src

RUN cargo build --release

ENTRYPOINT ["/usr/src/app/bin/sockrustet"]