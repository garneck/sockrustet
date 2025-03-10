FROM rust:1.85-slim-bullseye

WORKDIR /usr/src/app

COPY Cargo.toml Cargo.lock ./

RUN cargo build --release

ENTRYPOINT ["/usr/src/app/sockrustet"]