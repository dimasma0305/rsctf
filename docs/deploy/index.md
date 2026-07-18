# Choose a deployment

rsctf is one full-stack image: it serves the React interface and Rust API from the same binary. PostgreSQL is required, Redis is recommended for one replica and required for split roles, and uploaded files need persistent storage.

## Compare the supported paths

| | Docker Compose | Kubernetes with Helm |
| --- | --- | --- |
| Best for | One dedicated server or VM | An existing cluster and operations team |
| Database/cache | Included starter PostgreSQL and Redis | Included single-node starters or external services |
| Uploaded files | Named Docker volume | PersistentVolumeClaim |
| HTTPS | Included Caddy option or existing proxy | Ingress/controller supplied by the cluster |
| Dynamic challenges | Docker socket override | Kubernetes Pods + Services |
| A&D VPN | Most complete path | Advanced; requires privileged networking and cluster routing |
| Bring Your Own Container (BYOC) | Docker path | Not supported by the current Kubernetes backend |
| Default privilege | No Docker socket; `all` has checker-firewall `NET_ADMIN` | Round-engine Pod has checker-firewall `NET_ADMIN`; `web` is non-root |

For most events, use [Docker Compose](./docker). Choose [Kubernetes](./kubernetes) only if the team already operates the cluster and can test the rsctf-specific challenge networking.

## Deployment profiles

Think of the deployment as three layers:

1. **Platform only** — accounts, teams, games, static attachments, flag submission, scoring, and administration.
2. **Dynamic challenges** — adds Docker socket access or Kubernetes workload RBAC.
3. **A&D networking** — adds WireGuard, TUN access, public UDP, and a singleton background role. (`NET_ADMIN` is already required on round-engine roles for fail-closed checker egress.)

Enable only the layers the event uses. Each extra layer expands the operational and security boundary.

## Required persistent data

Back up both:

- **PostgreSQL**, which contains users, games, scores, settings, tokens, and repository credentials
- **The file storage root**, which contains uploaded attachments, avatars, writeups, build data, and related blobs

Redis is intentionally a disposable cache and does not need a data backup.

## Start as one, split when measured

The default `RSCTF_ROLE=all` remains one replica and owns the full platform.
Larger installations can run the same image as public `web` replicas plus one
`control`, or as `web` + active-active `engine` workers + one `network` owner.
The `migrate` role is a one-shot database upgrade command.

Split roles require shared Redis, storage, secrets, and container backend. The
integrated VPN/BYOC network owner remains exactly one replica. See
[Scale the single binary](./scaling) before increasing a replica count.
