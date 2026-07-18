# Reverse proxy and HTTPS

Use HTTPS for every Internet-facing rsctf deployment. Login sessions use secure cookies by default, browser mutations enforce same-origin checks, and WebSocket connections need the correct public origin.

## Required proxy behavior

Your proxy must:

- Terminate or pass through HTTPS
- Forward normal HTTP requests to rsctf port 8080
- Support WebSocket upgrades for `/hub` and Bring Your Own Container (BYOC) relay paths when used
- Preserve the original `Host` and scheme
- Apply sensible request/body timeouts for attachments and long-lived upgrades
- Avoid caching authenticated API responses

In a split-role deployment, send ordinary traffic to the web pool and route
BYOC agent/image plus `/hub/containerExec` paths to the singleton
control/network owner. The maintained path list and Caddy example are in
[Scale the single binary](./scaling#route-stateful-connections-to-the-network-owner).

Set `RSCTF_PUBLIC_URL` to the exact browser origin, for example:

```dotenv
RSCTF_PUBLIC_URL=https://ctf.example.org
RSCTF_COOKIE_SECURE=true
```

Do not add a trailing path. Use local HTTP only for development, with `RSCTF_COOKIE_SECURE=false`.

## Trusted forwarded addresses

rsctf ignores forwarded client-IP headers unless the immediate proxy is inside `RSCTF_TRUSTED_PROXY_CIDRS`.

Prefer the proxy's exact stable `/32` or `/128` address. A dedicated proxy-only Docker or Pod network can be trusted as a CIDR when no untrusted workload can join it.

```dotenv
RSCTF_TRUSTED_PROXY_CIDRS=172.30.0.2/32
```

::: danger Do not trust every private address
A broad value such as `10.0.0.0/8` can let another internal workload forge client IPs, weakening rate limits and anti-cheat attribution.
:::

Invalid CIDRs fail startup. An empty value is fail-closed: requests still work, but rsctf sees the proxy as the client address.

## Existing Traefik

Connect the rsctf service to a dedicated Traefik network and route its public hostname to internal port 8080. The root Compose file contains one homelab-specific example, including `websecure` and a `letsencrypt` resolver. Rename those labels to match your Traefik installation.

## Included Caddy option

The generic Docker wizard can add the included Caddy override. It is the easiest production path when ports 80/443 are free and the domain resolves directly to the server.

## Verify the boundary

```bash
curl -I https://ctf.example.org/
curl -fsS https://ctf.example.org/healthz
```

Then sign in through the public URL, refresh, perform a harmless profile update, and open a live page that uses WebSockets. Test only the canonical URL; mixing direct-IP HTTP and public HTTPS can produce expected origin/cookie failures.
