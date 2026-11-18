# Native WireGuard regression gate

Required CI jobs `vpn-client-profiles` and `vpn-native-clients` cover two players
on the same team. Failures block the `required` aggregate check. They run whenever
server checks are selected, including publication workflows.

The Linux fixture test migrates a disposable PostgreSQL schema, seeds two accounts
on one accepted team in live A&D and KoTH events, and concurrently calls the real
Toolkit VPN download route. It verifies distinct keys and addresses, both peers in
the Linux WireGuard kernel, stable re-downloads, and identity preservation when the
API VPN gate changes. It exports those actual profiles after deleting its schema.

Each OS job imports the first profile into a native client:

- Windows Server 2025: official, checksum-pinned and signature-verified WireGuard
  0.5.3 tunnel service (WireGuardNT).
- macOS 15: Homebrew `wg-quick` / `wireguard-go`, using a real utun interface.
  Installed versions are recorded in the job log. This does **not** exercise the
  App Store application's Network Extension or its import UI.

A Go helper hosts a disposable WireGuard hub and the second teammate. Each has an
independent userspace IP stack; their encrypted UDP endpoints bind only loopback.
Two native interfaces on one runner would share its routing table and would not
model two separate players. The helper uses the second RSCTF-generated profile,
continuously sending TCP and UDP while the native client connects, transfers data,
disconnects and reconnects three times. The test also checks explicit VPN DNS and
the native OS resolver against the platform's private ingress address. A failed
companion probe, stalled traffic, incorrect response, missing artifact or failed
native client fails the job. Neither job silently substitutes a mock client.

The fixture contains freshly generated **CI-only** keys, a loopback endpoint and
private test subnets; no deployment credentials are used. The same-run artifact is
bound to the tested source SHA and retained for one day. Result artifacts contain
only platform, commit and traffic counters. Client profiles are removed on exit.
Run only on disposable machines: the native test changes routes and DNS temporarily.

## Local checks

Use the Go version in `go.mod`:

```sh
cd tests/vpn-clients
go mod verify
go vet ./...
go build -mod=readonly -o vpn-fixture.exe .
RSCTF_VPN_SOURCE_SHA=<fixture-commit> node run.mjs fixture.json ./vpn-fixture.exe
```

The Node harness also supports Linux `wg-quick` for local end-to-end validation.
It requires passwordless `sudo`, WireGuard tools, an OS DNS integration such as
`resolvconf`, and a fresh namespace without conflicting test routes/interfaces.
Use the environment in the `vpn-client-profiles` CI job to generate `fixture.json`:

```sh
scripts/bounded-cargo.sh test --locked --lib \
  services::ad::vpn::client_tests::native_client_profiles_keep_teammates_independent \
  -- --exact --ignored --test-threads=1 --nocapture
```

That Rust test needs PostgreSQL, NET_ADMIN/NET_RAW, `wg`, iptables, ipset and iproute2.
It also needs the directly connected service interface and IP forwarding configured
by the CI job; a default route to the service CIDR correctly fails RSCTF's checks.
Run it separately from other environment-mutating VPN tests. Remove its `wg0`
interface (or discard the namespace) before starting the native client helper,
which uses the same disposable UDP port.

These tests detect shared identities and native transport/DNS regressions. They do
not reproduce a public network, a particular NAT/firewall, congested event traffic,
all desktop OS versions, or the deleted competition server. Passing CI cannot
establish the cause of that incident. Linux exercises RSCTF's actual kernel peer
installation; native OS jobs exercise its downloaded profiles against a userspace
hub, not the complete RSCTF service firewall or HTTPS/SSH applications.
