FROM rust:alpine AS builder
LABEL authors="randomairborne"

RUN apk add nasm musl-dev

WORKDIR /build/
COPY . .

RUN cargo build --release

FROM scratch

COPY --from=builder /build/target/release/asset-squisher /usr/bin/asset-squisher
