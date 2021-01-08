FROM rust:slim as builder
WORKDIR /app
ADD src/ src
ADD Cargo.toml .
RUN cargo build --release

FROM debian:stable-slim
COPY --from=builder /app/target/release/tool /usr/bin/tool
CMD ["/usr/bin/tool"]