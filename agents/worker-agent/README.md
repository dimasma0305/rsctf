# RSCTF trusted worker agent

The worker agent connects a dedicated Linux Docker host or VM outbound to
RSCTF. It does not open a public listener and does not expose the Docker API
through the tunnel. Native Windows-container execution is disabled in v1; use a
dedicated Linux VM when the physical host is a Windows PC.

## Enroll

```sh
printf '%s\n' "$ONE_TIME_TOKEN" | rsctf-worker-agent enroll \
  --server-url https://ctf.example \
  --token-stdin \
  --state-dir ./rsctf-worker
```

Enrollment generates the private key on the worker and submits only a CSR. On Unix,
the key is created with mode `0600`. `--token-stdin` keeps the one-time secret out of
the process command line; `--token-file` is also available for a protected temporary
file. The enrollment token is not saved or printed.

## Run

```sh
rsctf-worker-agent run \
  --config ./rsctf-worker/worker.json \
  --accept-host-network-boundary
```

The default Docker transport is `/var/run/docker.sock`; a custom local Unix
socket can be set with `--docker-endpoint`. Remote unauthenticated Docker TCP
endpoints are deliberately not supported.

Capacity is detected from Docker with host headroom. It can be pinned using
`--cpu-millis`, `--memory-bytes`, and `--slots`; overrides can reserve headroom
but cannot advertise more than the detected safe CPU, memory, or isolated
workload-network capacity. Replica capacity is advertised separately from the
smallest detected Docker network endpoint budget.
Placement labels use repeated `--label key=value` arguments. Production also
requires a quota-capable writable layer (for example
`overlay2` on XFS with `pquota`); `--allow-unbounded-storage` is only for trusted
disposable tests.

The agent requires TLS 1.3 mutual authentication and distinct control/data ALPNs. A
bounded control queue carries reconciliation commands. The data lane uses yamux and
accepts only server-opened, typed streams for an assigned workload, service, and named
port.

Before a workload reports `Ready`, every declared TCP port on every running replica
must accept a connection. Positive container readiness is cached rather than probed on
every inventory heartbeat. A failed proxy dial invalidates that container's cached
result, causing the next inventory pass to probe it again. This is a bounded startup
and recovery reachability check, not continuous or configurable application health
monitoring.

## Current runtime boundary

Docker execution supports immutable registry digests and worker-local image IDs,
multi-service workloads, stateless Jeopardy replicas, inventory/adoption,
generation replacement, unpublished internal-network ports, cached TCP
forwarding, and a fenced flag-write runtime primitive reserved for future
competitive-mode integration. Docker internal networks block routed
LAN/Internet egress but still expose the Linux bridge's host-side gateway; the
mandatory acknowledgement means the agent must run on a dedicated, firewalled
host or VM. Interactive exec, local image build, Kubernetes workload execution,
and native Windows containers are not advertised.
