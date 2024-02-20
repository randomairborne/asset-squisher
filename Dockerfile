FROM rust:alpine AS builder
LABEL authors="randomairborne"

WORKDIR /build/
COPY . .

RUN cargo build --release

FROM alpine:latest

COPY --from=builder /build/target/release/asset-squisher /usr/bin/asset-squisher
