FROM rust:1.88-slim AS builder

WORKDIR /build

RUN apt-get update && \
    apt-get install -y pkg-config libssl-dev clang && \
    rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --release --package sorter2-server

FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /build/target/release/sorter2-server /app/sorter2-server

RUN mkdir -p /data

ENV SORTER2_DATA_DIR=/data
ENV SORTER2_EVENT_LOG=/data/events.jsonl
ENV PORT=8080

EXPOSE 8080

CMD ["/app/sorter2-server"]
