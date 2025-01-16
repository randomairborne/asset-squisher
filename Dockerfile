FROM rust:alpine AS builder

RUN apk add musl-dev

WORKDIR /build

COPY . .

RUN cargo build --release

FROM alpine:latest

COPY --from=builder /build/target/release/asset-squisher /usr/bin/asset-squisher
