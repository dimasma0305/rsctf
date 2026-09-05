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

## DNS-stage verification and cleanup

- Fourteen focused deployment/development tests passed.
- Compose syntax and the Compose security-validation script passed.
- The deployed DNS image remained pinned to its existing immutable image:
  `sha256:97efff2babf85310d6f87bd969ac64d7006bbbadb147c90579d68d57634750f4`.
- Backend health remained healthy; public `/healthz` returned exactly `ok`.
- Diagnostic events 37–40, their accounts, teams, VPN peers, echo process and
  network namespaces were removed. Earlier attempts exposed fixture setup issues,
  not measured UDP transport loss; only event 40 completed the transport run.
- At this DNS-repair stage, no Rust or frontend artifact changed, so no
  Cargo/frontend rebuild was needed.
  `tcp.1pc.tf` was not redeployed as part of this scoped repair.

No claim is made that every challenge or Internet connection is now stable.
Remote-client UDP measurements during actual busy periods remain necessary.

## Follow-up: personal player profiles

A hidden regression event (44) reproduced a separate identity problem: with the
API VPN gate off, two teammates downloaded the same address/key from the
A&D/KotH Toolkit. The test stopped before installing the duplicate interface.
One WireGuard identity is not a multi-device pool; concurrent teammates must
not use the BYOC hosting peer as their player profile.

Toolkit downloads now provision personal peers regardless of the API VPN gate.
Issuance and kernel retention use the same event eligibility rule. Event expiry,
deletion, roster/account revocation and exact-target firewall boundaries remain
enforced. BYOC hosting bundles retain their separate participation peer. The
installer generates a missing credential-encryption key and preserves existing
keys; Toolkit-only profiles do not depend on a proof URL.

Intechfest's pool was expanded from `10.13.42.0/24` to `10.13.42.0/23`, retaining
the same hub address. Its Traefik return route was updated live and in the local
Compose configuration without restarting Traefik. This increases allocatable
peer addresses from 253 to 509. The 68 reserved historical personal addresses
were not recycled. This is an address-capacity increase, not proof of supported
concurrent traffic volume. Players must replace old team-shared profiles; their
BYOC host profile should not be installed on player devices.

The first backend rollout failed because a handwritten test table used a Rust
field name instead of the database's quoted `"Type"` column. The previous binary
was restored, including correcting access permissions on the rollback mount.
The fix now uses the real column name; read-only `EXPLAIN` checks against the
deployed schema cover provisioning, retention and Toolkit queries. This rollout
briefly interrupted service; no event or live VPN peer was active at the time.

The failed 150-player capacity attempts remain **inconclusive**: one had an
incorrect echo-service fixture and the corrected attempt stopped at 22 installed
peers on the 80% CPU safety ceiling. Its 2,378 TCP and 2,378 UDP echoes completed,
but this does not establish a 150-player capacity result or a 22-player limit.

A separate 30-second sample during the follow-up build measured average host
non-idle time of 47.3% (including 3.0% steal). An earlier one-second steal spike
was not sustained in that window. Linux records steal as involuntary CPU wait;
see the [kernel `/proc` documentation](https://www.kernel.org/doc/html/latest/filesystems/proc.html).
These samples cannot identify the cause of a particular player's disconnect.

### Corrected rollout and six-player transport test

The corrected local backend was deployed only to Intechfest. The running
executable and the built file both hash to:

```text
31c84301be5c6452fea396afa8000b9adb905686a2fd4ca533907950f30fb918
```

This is a source-development binary mounted into the existing runtime image,
not a new immutable production release. `tcp.1pc.tf` was not redeployed and no
remote push was performed. The backend reported healthy with zero restarts
after the corrected rollout, no new error/panic log matches, and exact public
health body `ok`. DNS namespace and UDP/TCP resolution checks passed.

Hidden event 45 used six synthetic players in two teams of three. Every player
joined through the API and downloaded their own Toolkit profile. The database
contained six distinct keys and addresses. Repeated downloads retained the same
credential; switching the API VPN gate on and off preserved those identities.
With the gate on, both download locations returned identical profiles. Anonymous
and nonparticipant requests were denied. Profiles contained the expanded split
routes and no default route.

Each peer sent five 1,200-byte echo messages/second over each of TCP and UDP,
through WireGuard and the event firewall. After 30 seconds at six peers, only
the first player's test interface received `netem` outbound delay of
`80ms ±20ms` and `2%` random loss for 45 seconds, followed by 20 seconds of
recovery. No host/live VPN interface received traffic shaping.

| Measurement | Result |
| --- | --- |
| Baseline, including ramp | 1,015/1,015 TCP and UDP replies |
| Entire run, TCP | 3,205/3,205 replies, no disconnects or application errors |
| Entire run, UDP | 3,198/3,205 replies; all seven missing datagrams belonged to the deliberately impaired player |
| Five unimpaired players | 2,641/2,641 replies per protocol, zero loss/disconnects |
| Highest unimpaired-player p95 over the run | TCP 5.17ms; UDP 5.16ms |
| TCP retransmits / timeouts | 2 / 0 |
| Kernel UDP receive/send buffer errors | 0 |
| Public health | 58/58 passed; slowest 119.8ms |
| Peak sampled host CPU, including steal | 64.8%; abort threshold 80% |

The added delay/loss did not disconnect the impaired TCP session or cause loss
for the other five players. UDP does not promise retransmission; missing
datagrams on a lossy path still need application-level tolerance. These are
server-local transport results, not remote ISP or 150-player certification.

Verification: warning-free bounded `cargo build --all-targets`, complete default
`cargo test` (1,702 passed, 408 environment-dependent tests ignored), both focused
PostgreSQL tests, five VPN deployment/contract tests, installer bootstrap tests,
format/diff checks, and Compose security validation passed. No frontend code was
changed. Diagnostic events 44–45, their synthetic users/teams/peers, network
namespace, echo/driver processes, and the disposable PostgreSQL test database
were removed. Historical user credentials were preserved.
