# VPN transport validation — 2026-09-05

Target: `intechfest.1pc.tf`, source-development backend. No other deployment was
restarted. This was a bounded server-side transport check, not an Internet-path
or full-capacity certification.

## Confirmed fault and repair

The DNS companion retained a replaced backend's network namespace. The current
backend's `10.13.42.1:53` refused requests; DNS in the obsolete namespace returned
`SERVFAIL` and had no default route. Recreating only `event-vpn-dns` restored both
the private event hostname and upstream resolution over UDP and TCP.

Both Compose definitions now request dependent restarts on explicit backend
updates. Backend-only replacements still require recreating the DNS companion;
`scripts/check-event-vpn.mjs` detects mismatched namespaces and tests resolution.
Docker documents the explicit-operation limitation in its
[dependency lifecycle documentation](https://docs.docker.com/compose/how-tos/startup-order/).

DNS stays bound to the VPN hub, without a published host port. Forwarding has a
256-request concurrency bound, in addition to existing CPU, memory and process
limits. Excess outstanding forwarding requests are refused rather than allowed
to grow without bound; see [CoreDNS forwarding](https://coredns.io/plugins/forward/).

## UDP evidence

Eight separate WireGuard keys and Linux network namespaces connected through the
current backend's UDP listener. Accounts joined a hidden diagnostic event through
the normal API and downloaded personal profiles. The echo endpoint was limited
to that event's declared service address/port by the real VPN firewall. No
challenge exploit or player scoring was involved.

The WireGuard outer sockets originated on this VPS, using its loopback-published
VPN port. This exercised encryption, Docker UDP ingress, routing, firewall policy
updates and the return path, **not a remote player's ISP, Wi-Fi or path MTU**.

| Phase | Echo requests/replies | Loss | Highest per-client p95 RTT |
| --- | ---: | ---: | ---: |
| Warmup | 10/10 | 0 | 8.47 ms |
| Existing peer while seven peers were added | 1,500/1,500 | 0 | 6.50 ms |
| Eight peers, 64-byte payload | 4,000/4,000 | 0 | 4.41 ms |
| Eight peers, 1,200-byte payload | 4,000/4,000 | 0 | 3.05 ms |
| Eight peers, 1,392-byte payload | 4,000/4,000 | 0 | 3.64 ms |

Steady traffic was 50 requests/second/peer (400 total), with a 1,420-byte tunnel
MTU. Peak sampled host CPU was 71.4%; the diagnostic stopped clients if a sample
exceeded 80%. Kernel UDP receive/send buffer-error counters remained zero.
The test peer could not connect to the unrelated backend TCP port 8080.

## Verification and cleanup

- Fourteen focused deployment/development tests passed.
- Compose syntax and the Compose security-validation script passed.
- The deployed DNS image remained pinned to its existing immutable image:
  `sha256:97efff2babf85310d6f87bd969ac64d7006bbbadb147c90579d68d57634750f4`.
- Backend health remained healthy; public `/healthz` returned exactly `ok`.
- Diagnostic events 37–40, their accounts, teams, VPN peers, echo process and
  network namespaces were removed. Earlier attempts exposed fixture setup issues,
  not measured UDP transport loss; only event 40 completed the transport run.
- No Rust or frontend artifact changed, so no Cargo/frontend rebuild was needed.
  `tcp.1pc.tf` was not redeployed as part of this scoped repair.

No claim is made that every challenge or Internet connection is now stable.
Remote-client UDP measurements during actual busy periods remain necessary.
