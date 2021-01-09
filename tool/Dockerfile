FROM rust:1.49-slim as builder
WORKDIR /app
ADD Cargo.toml .
RUN mkdir src && echo 'fn main(){}' > src/main.rs && cargo build --release && rm -rf ./src
ADD src/ src
RUN touch src/main.rs && cargo build --release

FROM debian:stable-slim
COPY --from=builder /app/target/release/tool /usr/bin/tool
CMD ["/usr/bin/tool"]