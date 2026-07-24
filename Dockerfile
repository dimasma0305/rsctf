ARG RSCTF_DEFAULT_BYOC_AGENT_IMAGE=""
ARG RSCTF_DEFAULT_BYOC_AGENT_MULTIARCH="false"

# --- React frontend build stage ---
# The frontend output is architecture-independent. Build it natively on the
# BuildKit host instead of running Node under QEMU for arm64 release images.
FROM --platform=$BUILDPLATFORM node:22-bookworm AS web-builder
RUN corepack enable
WORKDIR /web
COPY web/package.json web/pnpm-lock.yaml web/pnpm-workspace.yaml ./
RUN --mount=type=cache,target=/root/.local/share/pnpm/store \
    pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

# --- backend build stage ---
FROM lukemathwalker/cargo-chef:0.1.77-rust-1-bookworm@sha256:1689f62cfaa6603480356923cb5966544b2dd6ea523e30486bee4f149965d5bc AS chef
# libpcap-dev is needed to build the live traffic-capture (pcap) crate.
RUN apt-get update \
    && apt-get install -y --no-install-recommends libpcap-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app

FROM chef AS planner
COPY Cargo.toml Cargo.lock* build.rs ./
COPY lib/worker-protocol ./lib/worker-protocol
COPY scripts/bootstrap-worker.sh scripts/bootstrap-worker.ps1 ./scripts/
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS dependency-builder
COPY --from=planner /app/recipe.json recipe.json
COPY lib/worker-protocol ./lib/worker-protocol
# cargo-chef 0.1.77 still emits target-level `edition`, which current Cargo
# deprecates. The package-level edition remains intact; remove only the two
# synthetic target fields before cooking the dependency layer.
RUN sed -i 's/\\nedition = \\"2021\\"\\nrequired-features/\\nrequired-features/g' recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo chef cook --release --locked --recipe-path recipe.json

FROM dependency-builder AS builder
COPY Cargo.toml Cargo.lock* build.rs ./
COPY scripts/bootstrap-worker.sh scripts/bootstrap-worker.ps1 ./scripts/
COPY src ./src
ARG RSCTF_DEFAULT_BYOC_AGENT_IMAGE
ARG RSCTF_DEFAULT_BYOC_AGENT_MULTIARCH
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    RSCTF_DEFAULT_BYOC_AGENT_IMAGE="${RSCTF_DEFAULT_BYOC_AGENT_IMAGE}" \
    RSCTF_DEFAULT_BYOC_AGENT_MULTIARCH="${RSCTF_DEFAULT_BYOC_AGENT_MULTIARCH}" \
    cargo build --release --locked \
    && cp /app/target/release/rsctf /tmp/rsctf

# --- runtime stage ---
FROM debian:bookworm-slim
# git: the repo-binding challenge-sync (git_sync) shells out to `git clone`/`fetch`.
# ca-certificates: TLS for git-over-https + outbound HTTP. libpcap0.8: live capture.
# iptables + ipset + iproute2: the in-process A&D WireGuard hub enforces
# game-scoped peer/target sets and scoped masquerading (needs NET_ADMIN,
# NET_RAW for the iptables ipset matcher, and the host wireguard/ipset modules).
# python3 + venv: A&D checkers are prepared as a venv on sync and run as a
# sandboxed subprocess (Landlock + seccomp + dropped uid). The venv-provided pip
# may install exact requirements.txt pins from binary wheels only; source builds
# and their setup.py/PEP 517 hooks stay disabled.
# Checker children use a reserved configurable UID range; no passwd entries are
# required because the launcher switches numeric identities directly.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git libpcap0.8 iptables ipset iproute2 wireguard-tools \
       python3 python3-venv \
    && rm -rf /var/lib/apt/lists/*
ARG RSCTF_DEFAULT_BYOC_AGENT_IMAGE
LABEL org.opencontainers.image.rsctf.byoc-agent="${RSCTF_DEFAULT_BYOC_AGENT_IMAGE}"
WORKDIR /app
COPY --from=builder /tmp/rsctf /usr/local/bin/rsctf
COPY --from=web-builder /web/build /app/web/build
COPY LICENSING.md LICENSE.txt NOTICE /app/web/build/legal/
COPY web/src/lib/creepjs/LICENSE /app/web/build/legal/third-party/CreepJS-LICENSE.txt
ENV RSCTF_BIND=0.0.0.0:8080
ENV RSCTF_STATIC_DIR=/app/web/build
EXPOSE 8080
# Optional trusted worker-plane mTLS listener.
EXPOSE 9443
# WireGuard hub UDP port teams dial (A&D VPN).
EXPOSE 51820/udp
ENTRYPOINT ["/usr/local/bin/rsctf"]
