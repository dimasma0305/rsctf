# syntax=docker/dockerfile:1
# Disposable benchmark/runtime-test tools. The server is a separately hashed,
# read-only mounted release binary, so both revisions use this exact same image.
FROM debian@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
RUN --mount=type=secret,id=system_ca,required=true \
    apt-get -o Acquire::https::CaInfo=/run/secrets/system_ca update \
    && apt-get -o Acquire::https::CaInfo=/run/secrets/system_ca install -y --no-install-recommends \
       ca-certificates libpcap0.8 python3-venv git iproute2 iptables ipset \
       curl procps netcat-openbsd \
    && rm -rf /var/lib/apt/lists/*
ENTRYPOINT ["/usr/local/bin/rsctf"]
