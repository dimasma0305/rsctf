# Health and troubleshooting

Start with the smallest failing layer. Record the UTC time and exact error before restarting anything.

## Quick checks

### Docker

```bash
cd deploy
docker compose ps
docker compose logs --tail=200 rsctf db redis
docker compose config --quiet
curl -v --max-time 5 http://127.0.0.1:8080/healthz
```

### Kubernetes

```bash
kubectl -n rsctf-system get pods,svc,ingress,pvc,events
kubectl -n rsctf-system describe deployment rsctf
kubectl -n rsctf-system logs deployment/rsctf --tail=200
kubectl -n rsctf-system get events --sort-by=.lastTimestamp
```

Resource names may include the Helm release prefix. Use `kubectl get deploy -n rsctf-system` to find the exact name.

## Startup errors

### JWT secret rejected

**Symptom:** rsctf exits with a message about a short or known secret.

Generate at least 32 random bytes and update the deployment Secret/environment:

```bash
openssl rand -hex 32
```

Preserve the old value during ordinary updates. Replacing it invalidates existing sessions.

### PostgreSQL connection failed

Check the database container/Pod, URL hostname, database name, user, password, and network. In Compose, use the service hostname `db`, not `localhost`, from inside the rsctf container.

If you changed the environment password after the database volume was first initialized, restore the old value or change the role password inside PostgreSQL.

### Migration failed

Save the full migration error and stop repeated restarts. Back up the database before manual intervention. Migrations are forward-only; do not edit an already shipped migration or delete the schema history to “retry.”

### Redis unavailable

rsctf falls back to an in-memory cache and logs a warning. The site can remain available on one replica, but load and cache consistency change. Restore Redis and restart rsctf if the connection is only established at startup.

## Browser and proxy problems

### Login works, then disappears or mutations return 403

Check:

- You opened the exact `RSCTF_PUBLIC_URL` origin.
- Production uses HTTPS with secure cookies.
- Local plain HTTP explicitly uses `RSCTF_COOKIE_SECURE=false`.
- The proxy forwards the original host and scheme.
- You are not switching between an IP address, internal hostname, and public domain.

### Every player appears to have the proxy IP

Set `RSCTF_TRUSTED_PROXY_CIDRS` to the immediate proxy's exact address or a dedicated proxy-only network. Do not trust a broad internal range merely to make the displayed IP look correct.

### WebSocket/live updates fail

Confirm that the reverse proxy permits HTTP upgrade connections and does not impose a short idle timeout on `/hub` or Bring Your Own Container (BYOC) upgrade paths. Test the browser developer console and proxy logs at the same UTC time.

## Container problems

### “No container backend configured”

The platform-only profile intentionally sets `RSCTF_CONTAINER_BACKEND=none`. Re-run the wizard and enable the Docker challenge backend, accepting the Docker-socket risk, or deploy the Kubernetes backend with the chart RBAC.

### Docker daemon unavailable

Check that `/var/run/docker.sock` exists on the host, the correct override is selected, and the socket is mounted into rsctf. Rootless or remote Docker needs an explicit, tested `DOCKER_HOST` and is not supported by the standard VPN profile.

### Challenge starts but players cannot connect

Verify `RSCTF_DOCKER_PUBLIC_ENTRY` or `RSCTF_K8S_PUBLIC_ENTRY`, the displayed port, host/cloud firewall rules, and NAT. Normal Docker challenges publish a random host port; normal Kubernetes challenges use a random NodePort.

### Kubernetes challenge is forbidden

Inspect the rsctf ServiceAccount, RoleBinding in the challenge namespace, and API error. The current backend needs Pods, Services, and NetworkPolicies. Use the Helm chart's RBAC rather than the legacy sample files.

## A&D/VPN problems

### Checker-owning role exits during confinement preflight

Do not bypass the preflight. The role runs a real no-network child before its
topology heartbeat or readiness transition. Confirm the Linux kernel exposes
Landlock ABI v3, `CONFIG_SECURITY_LANDLOCK` is enabled in the active LSM set,
seccomp filtering is available, the checker UID interval is unused, `/tmp` is
writable, and the container retains `CHOWN`, `SETUID`, `SETGID`, `KILL`, and
`NET_ADMIN`. The error distinguishes launcher confinement from the uid-scoped
iptables/ip6tables owner chain. On Kubernetes, also set the real cluster
Service CIDR in `kubernetes.adServiceCidr`/`RSCTF_K8S_AD_SERVICE_CIDR` on every
non-migration role; VPN does not need to be enabled for this requirement.

### `/dev/net/tun` missing or WireGuard initialization fails

VPN mode requires Linux, rootful containers, the TUN device, `NET_ADMIN`,
`NET_RAW` for the iptables ipset matcher, WireGuard and ipset kernel support,
and IPv4 forwarding. Run installer `doctor`, inspect host kernel logs, and
validate the chosen CIDRs do not overlap local routes.

### VPN connects but targets are unreachable

Compare the routes in the generated profile with the configured service CIDR. Verify the rsctf container/Pod can route to the A&D service network, firewall/NAT rules exist, and the target service is healthy. Kubernetes additionally needs the real cluster Service CIDR and cluster-specific routing.

### Second replica fails in VPN mode

Do not scale an `all`, `control`, or `network` role: exactly one process owns the
PostgreSQL singleton lease and kernel network state. Scale `web` and `engine`
roles instead. Keep VPN intent enabled on them so policy mutations wait for the
owner's durable acknowledgement; the maintained manifests grant TUN and
`NET_ADMIN` plus the ipset matcher's `NET_RAW` only to the singleton owner.

## Player problems

### Cannot join a game

Confirm the account is active, the user belongs to a team, team size is allowed, the correct division and invite code were selected, and the game is visible. A valid request may remain Pending until an organizer accepts it.

### Team membership cannot change

An accepted game participation can lock the roster. Ask the organizer to review the participation; repeatedly rotating invite codes does not bypass an event lock.

### Correct-looking flag is rejected

Copy the entire flag without extra whitespace, preserve case, and verify it belongs to the correct challenge/team/window. For A&D, confirm the configured lifetime and target. Send details privately without including the flag in a public channel.

### KotH capture does not appear

Confirm that you fetched the capability for the correct hill after its latest crown-cycle reset, used the current exact endpoint and marker path, and are not in champion cooldown. Check that the reset phase is `Active`, capability issuance is ready, and the functional checker returns `Ok`. The first valid observation is provisional; the same team and capability must remain in control for the configured consecutive healthy checks before the board shows a confirmed king or awards acquisition credit. A rival capability, `Mumble`, or `Offline` breaks the confirmation streak, while reset/readiness time and platform `InternalError` results are void. A local write alone does not immediately confirm the claim.

## Still stuck?

Collect:

- rsctf version/image tag
- deployment type and selected optional features
- UTC timestamp
- relevant sanitized logs
- browser/proxy HTTP status
- game/challenge/team identifiers without secrets

Open a GitHub issue only for non-sensitive defects. Send security findings and live event secrets through the repository's private security-reporting channel.
