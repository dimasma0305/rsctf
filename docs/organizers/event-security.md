# Event VPN and anti-cheat telemetry

rsctf can place one event behind its managed WireGuard network and collect a
small, bounded set of anti-cheat signals from that network. Every switch is off
by default. Enabling the event gate does not enable packet recording, and none
of the new network signals automatically proves that a player cheated.

## What is implemented

| Control or signal | What rsctf keeps | Use in a decision |
| --- | --- | --- |
| Event-only VPN gate | A short-lived proof bound to the account, participation, WireGuard peer generation, policy revision, and security stamp | Access control, not a cheat score |
| Aggregate behavior | Five-minute byte, packet, connection, active-time, and distinct-destination counters | Context for an investigation |
| Provider DNS category | Fifteen-minute count for a small built-in category such as OpenAI, Anthropic, another AI provider, or hosting infrastructure | Context only; zero points |
| Peer network profile | A keyed endpoint hash plus an optional operator-supplied ASN/network class | Context only; zero points |
| VPN profile sharing | More than one keyed endpoint observed for one peer | Context only; zero points |
| Real flag transport | A keyed hash when the exact bytes of a currently issued flag cross another team's VPN flow | Context until joined to canonical stolen-flag submission evidence |
| Per-participation variant | An auto-built or explicitly pinned immutable generator image, seed commitment, artifact hash, and frozen manifest | Makes answer sharing less reusable |
| Solve receipt | A one-use, expiring proof from a named trusted verifier, bound to the user, participation, challenge, answer, and variant | Required only for challenges configured to require it |
| Evidence graph | Links telemetry to its source row and to independently verified incidents | Explains corroboration; relationships do not add points |

There are no fake or canary flags in this design. The in-memory scanner receives
only real, platform-issued flag strings. It never stores those strings, packet
payloads, DNS names, or raw public endpoint addresses.

## Confidence and scoring

Do not describe a telemetry combination as “100% confidence.” Network data can
be incomplete, shared, encrypted, proxied, or caused by legitimate research.
The fused report groups evidence into independent families and shows one of five
bands: **No evidence observed**, **Context only**, **Watch**, **Investigate**, or
**Verified evidence**. It also displays reviewer confirmation separately.

AI-provider DNS, VPS/hosting source, multiple endpoint profiles, and observed
flag transport are created as shadowed Context/0 findings. They cannot increase
a score or corroboration count. A real foreign flag seen on the VPN is linked to
an immutable stolen-flag incident only if the same receiving participation later
submits the same owner's challenge flag. The canonical submission remains the
hard evidence; the network observation only explains provenance.

Use the organizer review action to mark a finding Explained, Suspicious,
Confirmed, Dismissed, or Needs more evidence. Explained and Dismissed actionable
findings do not contribute to the fused score. A reviewer confirmation is not
silently converted into an automatic certainty percentage.

## Enable an event safely

1. Deploy the managed WireGuard owner and configure three independent 32+
   character secrets: `RSCTF_EVENT_VPN_CREDENTIAL_KEY`,
   `RSCTF_EVENT_SENSOR_TOKEN`, and `RSCTF_SOLVE_RECEIPT_ISSUER_TOKEN`.
2. Set `RSCTF_EVENT_VPN_PROOF_URL` to an HTTPS rsctf origin whose exact address
   is routed through WireGuard. Keep it on the same browser origin so the
   session cookie is not shared with another site.
3. Set `RSCTF_EVENT_VPN_ALLOWED_IPS` to only the proof-origin `/32` and any exact
   event-service routes. `0.0.0.0/0` and `::/0` are rejected. rsctf never needs
   to carry all player Internet traffic.
4. Start the optional sensor sidecar only if telemetry is wanted. It receives
   `NET_RAW`, but no database credential, Docker socket, TUN administration, or
   writable root filesystem.
5. In **Edit game → Event Security**, enable **Require event VPN**. Enable each
   telemetry category separately. Save a reasoned policy change before the game
   starts.
6. Players download their event profile from the game page. The web client mints
   a new 30-second proof over the tunnel when a protected request needs it.

During `[start, end)`, game APIs, protected assets, and challenge proxy sessions
recheck the exact live peer source. A policy change, account/session revocation,
roster removal, challenge visibility change, or container replacement wins at
the final authorization boundary. Practice and archived views retain their
normal behavior. An administrator can create a short, audited emergency bypass
with a reason and explicit expiry.

## What the sensor can and cannot see

The sensor sees only traffic crossing the managed `wg0` interface.

- DNS categories require plaintext DNS on TCP/UDP port 53 through the tunnel.
  DNS-over-HTTPS, DNS-over-TLS, and traffic outside the split routes are not
  visible. This is why the result is context only.
- Real-flag matching works only when the exact flag bytes appear in plaintext
  TCP/UDP payloads crossing the tunnel. TLS, SSH, application encryption,
  out-of-order streams, and routes outside the VPN can hide them.
- ASN classification uses a local `CIDR,ASN,CLASS` prefix file supplied by the
  operator. Without it, rsctf can still compare keyed endpoint identities but
  does not guess an ASN from a remote service.
- Multiple endpoint hashes may mean profile sharing, roaming, carrier NAT, or a
  changed home network. It is never proof by itself.

## Resource and storage bounds

The sensor is deliberately lossy. Gameplay never waits for it.

- Packet capture is capped at a 4 MiB kernel buffer and 4,096-byte snapshots.
- At most 65,536 flow records, 50,000 flag patterns, 4 MiB of plaintext pattern
  bytes, two queued batches, and 4,096 rows per upload are kept.
- Completed behavior and DNS data are aggregated into five- and fifteen-minute
  buckets before upload.
- Database accounting stops telemetry at 256 MiB logical storage per event and
  5 GiB globally. Drop counters are aggregated by hour rather than creating one
  row per drop.
- The shipped sidecar limit is 0.5 CPU and 128 MiB RAM. If it falls behind, it
  drops telemetry and records bounded counters; it does not slow submissions,
  boards, containers, or VPN forwarding.
- An administrator can purge one event's raw telemetry with an audited reason.
  Findings and reviews remain append-only forensic records.

Run the fixed-rate resource test before enabling telemetry for a large event:

```sh
cd tests/load
EVENT_SECURITY_STRESS_ACK=1 GAME=10 RATE=2 ROWS_PER_BATCH=256 DURATION=60s \
  RSCTF_EVENT_SENSOR_TOKEN=... npm run event-security
```

The acknowledgement is required because this harness writes telemetry; use a
disposable event and purge it afterward. The runner compares an aggregate-only baseline with sensor ingestion at the
same request rate, samples rsctf/PostgreSQL CPU and memory, checks `/healthz`,
rejects unexpected 5xx responses, and verifies that logical storage never
exceeds the configured event quota. Record results in `tests/load/REPORT.md`;
compare CPU at the held rate, not noisy peak requests per second.

## Challenge variants and solve receipts

Per-participation variants are available only for Jeopardy challenges. Configure
`variantMode: PerParticipation` and place a `Dockerfile` in the challenge's
`generator/` directory. A trusted Repository Binding scan archives that source,
builds it, runs its contract twice with identical input, and records the local
immutable image ID. An unchanged rescan reuses that build; changing a generator
file queues another build. Deployments whose builders and control process do not
share a trusted Docker daemon can instead configure a matching
`image@sha256`/`sha256` pair explicitly. Generate and freeze variants before the
event starts. Generator output is size- and time-bounded, and the resulting
manifest is frozen. Variant policy and generator source cannot change after the
game starts.

If the same package owns an enabled container runtime, disable that challenge
before changing its generator source, rescan/build, and then re-enable it. This
keeps the published runtime archive and the generator build on one package
revision.

Solve receipts are also opt-in per Jeopardy challenge. A trusted verifier calls
the machine endpoint with its dedicated issuer token and returns the short-lived
receipt to the player. Submission consumes it once in the same transaction as
grading. A receipt is bound to the exact answer hash and canonical variant, so
copying it to another account or team does not work.

Neither mechanism claims to detect whether prose or code was written by an AI.
They make sharing less reusable and add trustworthy provenance around a solve.

For a copyable repository manifest, deterministic generator, contract test, and
pre-event API client, see the [sample challenge repository](./sample-repository)
and its
[provenance automation guide](https://github.com/dimasma0305/rsctf-challenges/blob/main/PROVENANCE.md).
