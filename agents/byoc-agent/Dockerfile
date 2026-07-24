FROM lukemathwalker/cargo-chef:0.1.77-rust-1-bookworm@sha256:1689f62cfaa6603480356923cb5966544b2dd6ea523e30486bee4f149965d5bc AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS dependency-builder
COPY --from=planner /app/recipe.json recipe.json
RUN sed -i 's/\\nedition = \\"2021\\"\\nrequired-features/\\nrequired-features/g' recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef cook --release --locked --recipe-path recipe.json

FROM dependency-builder AS builder
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked \
    && cp /app/target/release/rsctf-byoc-agent /tmp/rsctf-byoc-agent \
    && rm -rf /app/target

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /tmp/rsctf-byoc-agent /usr/local/bin/rsctf-byoc-agent
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/rsctf-byoc-agent"]
