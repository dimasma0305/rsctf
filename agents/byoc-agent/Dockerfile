FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked \
    && cp /app/target/release/rsctf-byoc-agent /tmp/rsctf-byoc-agent

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /tmp/rsctf-byoc-agent /usr/local/bin/rsctf-byoc-agent
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/rsctf-byoc-agent"]
