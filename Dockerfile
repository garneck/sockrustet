FROM rust:1.85-slim-bullseye

WORKDIR /usr/src/app

COPY src ./src

RUN cargo build --release

ENTRYPOINT ["/usr/local/bin/sockrustet"]