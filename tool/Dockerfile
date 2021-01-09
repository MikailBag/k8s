FROM rust:1.49-slim as builder
WORKDIR /app
RUN apt update && apt install -y libssl-dev pkg-config
ADD Cargo.toml .
RUN mkdir src && echo 'fn main(){}' > src/main.rs && cargo build --release && rm -rf ./src
ADD src/ src
RUN touch src/main.rs && cargo build --release

FROM debian:stable-slim
RUN apt update && apt install -y openssl
COPY --from=builder /app/target/release/tool /usr/bin/tool
CMD ["/usr/bin/tool"]