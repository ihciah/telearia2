FROM rust:1.75-alpine as builder
WORKDIR /usr/src/telearia2
RUN apk add --no-cache musl-dev libressl-dev

COPY . .
RUN CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse RUSTFLAGS="" cargo build --bin telearia2 --release

FROM alpine:latest

# By default telearia2 will use `config.toml` as config path.
ENV CONFIG_PATH=""

RUN apk add --no-cache ca-certificates
COPY --from=builder /usr/src/telearia2/target/release/telearia2 /usr/local/bin/telearia2
ENTRYPOINT ["/usr/local/bin/telearia2"]
