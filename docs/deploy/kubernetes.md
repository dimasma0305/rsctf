# Kubernetes with Helm

The published rsctf Helm chart deploys the platform, its HTTP Service,
persistent storage, and optional starter PostgreSQL/Redis services. It can also
grant rsctf narrowly scoped permission to create challenge Pods, Services, and
per-instance NetworkPolicies. Every runtime role uses the same image;
`runtimeRole: all` remains the default. A fresh installation does not need a
source checkout. Before upgrading an existing installation across an
incompatible schema release, obtain `scripts/kubernetes-maintenance-cutover.sh`
from the exact reviewed source release that produced the chart and image; do
not run an unversioned copy from another branch.

## Before installing

You need:

- Kubernetes with a default or selected StorageClass
- Helm 3 and `kubectl`
- An ingress controller or another way to expose the HTTP Service
- A reachable PostgreSQL database; the bundled instance is for a simple single-node start
- A CNI that enforces NetworkPolicy if you run untrusted dynamic challenges
- A published rsctf image for your CPU architecture

Check your current target:

```bash
kubectl config current-context
kubectl cluster-info
helm version
```

## Download the example values

Choose one released version for both the chart and image:

```bash
export RSCTF_VERSION=1.2.3
helm show values oci://ghcr.io/dimasma0305/charts/rsctf \
  --version "$RSCTF_VERSION" > rsctf-values.yaml
chmod 600 rsctf-values.yaml
```

The GitHub chart package must be public for anonymous pulls. If it is private, run `helm registry login ghcr.io` with an account that can read packages.

## Create private values

Do not put production secrets in the repository. Create a private values file outside version control:

```yaml
# rsctf-values.yaml
image:
  repository: ghcr.io/dimasma0305/rsctf
  tag: "1.2.3"

secrets:
  jwtSecret: "replace-with-at-least-32-random-bytes"
  identityHashKey: "replace-with-a-different-32-byte-random-value"

config:
  publicUrl: "https://ctf.example.org"
  cookieSecure: true

ingress:
  enabled: true
  className: nginx
  hosts:
    - host: ctf.example.org
      paths:
        - path: /
          pathType: Prefix
```

Generate each secret independently with `openssl rand -hex 32` and protect the
file with `chmod 600 rsctf-values.yaml`. Keep `identityHashKey` stable across
replicas, restarts, and JWT rotations; changing it breaks continuity for
privacy-preserving anti-cheat identity correlation.

If an ingress controller sets forwarded client-address headers, also set `config.trustedProxyCidrs` to the controller's actual source CIDR. Leave it empty until you know that range; trusting a broad cluster or private network lets other workloads spoof player IPs. See [Reverse proxy and HTTPS](./reverse-proxy).

The bundled starter PostgreSQL/Redis passwords and first-administrator setup
token are generated on first install and retained on upgrades. The Helm notes
print a command that reads the token without placing it in a URL. For
production, use `existingSecret.name` with an external secret manager and an
external PostgreSQL service instead of keeping sensitive values in Helm release
data; include the configured `identity-hash-key` and `bootstrap-token` keys.

## Install

The command below is only for a fresh installation with no old rsctf runtime
Pod. Do not use a plain `helm upgrade` for an existing release: migrations
0089–0091 require the enforced [maintenance cutover](./operations.md#update-helm)
so every old runtime is stopped before the new schema is applied.

```bash
helm upgrade --install rsctf oci://ghcr.io/dimasma0305/charts/rsctf \
  --version "$RSCTF_VERSION" \
  --namespace rsctf-system \
  --create-namespace \
  --values rsctf-values.yaml \
  --wait --wait-for-jobs
```

Then inspect the rollout:

```bash
kubectl -n rsctf-system get pods,svc,ingress,pvc
kubectl -n rsctf-system rollout status deployment/rsctf
kubectl -n rsctf-system logs deployment/rsctf --tail=200
```

The exact resource name includes the Helm release and chart naming rules; use `kubectl -n rsctf-system get deploy` if the example name differs.

## Dynamic challenge Pods

Set the chart's container backend to Kubernetes only after you understand the exposure model:

- rsctf creates challenge Pods and Services in a dedicated challenge namespace.
- Direct-mode challenge Services use random NodePorts.
- PlatformProxy Services use ClusterIP plus a per-instance ingress
  NetworkPolicy that admits only the configured rsctf control namespace, pod
  label, TCP port, and selected challenge Pod.
- `RSCTF_K8S_PUBLIC_ENTRY` must lead players to nodes where those ports are reachable.
- Direct `Isolated` NodePorts additionally require
  `kubernetes.isolatedIngressCidrs` (the post-NAT source ranges seen by Pods)
  and `kubernetes.podCidrs` (every cluster Pod range). rsctf subtracts Pod
  ranges from each ingress block and refuses ambiguous overlap, preventing a
  different challenge Pod from reaching the isolated workload. These Services
  use `externalTrafficPolicy: Local` to preserve the source address, so route a
  player's connection to the node currently hosting that challenge Pod.
- A&D services use ClusterIP and per-instance NetworkPolicies.
- KotH marker reads use the narrowly scoped `pods/exec` subresource.
- Private challenge image pull credentials are not currently attached to generated Pods.
- Challenge images must be portable repository digests. Configure
  `registry/name@sha256:...` directly unless a separate Docker-enabled build
  role resolves the mutable input tag; daemon-local archive builds cannot run on
  Kubernetes nodes.

The chart creates a ServiceAccount and a namespaced Role for only the resources the current backend uses.

The Kubernetes API accepting a NetworkPolicy object does not prove that the CNI
enforces it. Before enabling this backend, run a real cross-Pod probe in the
challenge namespace: the labeled rsctf control Pod must reach the challenge
port, while an otherwise ordinary Pod must time out. Then set:

```yaml
containerBackend: kubernetes
trafficCapture:
  enabled: false
kubernetes:
  adServiceCidr: 10.96.0.0/12
  isolatedIngressCidrs: [198.51.100.0/24]
  podCidrs: [10.244.0.0/16]
  networkPolicyEnforced: true
```

Both Helm validation and binary startup fail closed until that acknowledgement
is present. The challenge-namespace Role must retain `get`, `list`, `create`,
and `delete` on `networkpolicies.networking.k8s.io`; policy creation happens
before Pod creation. Rollback removes a policy created by the failing attempt
only when its new Pod is also removed; an idempotent retry never removes the
policy protecting an adopted Pod.

## Current Kubernetes limitations

Treat Kubernetes support as advanced and test the complete event flow. In the current backend:

- Docker-specific build, terminal, and snapshot paths are unavailable or limited.
- Live libpcap collection is Docker-only; set `trafficCapture.enabled: false`
  when using the Kubernetes container backend.
- Regular challenges depend on externally reachable NodePorts.
- The in-process BYOC yamux relay currently requires the Docker backend's
  shared isolated service network; Kubernetes-backed BYOC service relays are
  rejected. Managed A&D/KotH workloads remain supported.
- The network role is single-active. BYOC agent and container-hub paths require
  explicit Ingress/Gateway routing to that role.
- Split roles need a shared RWX filesystem with cross-client atomic rename and
  POSIX advisory-lock (`flock`) semantics for repository/checker/capture paths,
  even when blob assets use S3.

## A&D VPN in Kubernetes

The integrated WireGuard hub is an advanced, cluster-specific configuration. It
needs exactly one `all`, `control`, or `network` owner Pod, an ordinary isolated
Pod network namespace, `NET_ADMIN`, `/dev/net/tun`, permitted IPv4-forwarding
sysctls, `NET_RAW` for the iptables ipset matcher, a public UDP endpoint, the
actual cluster Service CIDR, and working
routing from the owner Pod to Service IPs. Split web/engine releases set the
same `vpn.enabled` intent so they can wait for durable policy acknowledgement,
but only the owner receives TUN, `NET_RAW`, forwarding, and the WireGuard Service. An
`engine` Pod still receives `NET_ADMIN` solely to install the process checker's
uid-scoped egress firewall; a `web` Pod receives no kernel capability.
Every non-migration release using `containerBackend: kubernetes` must set
`kubernetes.adServiceCidr` to the cluster's real Service CIDR, even when VPN is
off. Web provisioning consumes it when creating A&D/KotH policy, while checker
owners use it to reject targets outside the service network. Checker-owning
nodes must also expose Landlock ABI v3 as an active LSM and seccomp filter
support; each Pod proves the real child confinement path before readiness.
Every such release must also set `kubernetes.networkPolicyEnforced: true` only
after the enforcement probe above succeeds.

Managed clusters with restricted Pod Security may reject this mode. A NetworkPolicy-capable CNI is necessary but not sufficient; verify routing and isolation with two real test teams.

`vpn.sensor.enabled: true` adds the bounded event sensor to the singleton VPN
owner Pod. Helm rejects a sensor without `vpn.enabled`. Configure
`vpn.eventProofUrl` as an HTTPS rsctf origin and put only its exact `/32` plus
required event-service routes in `vpn.eventAllowedIps`; default routes are
rejected. The sidecar has `NET_RAW` but no database secret or `NET_ADMIN`, and
ships with 0.5 CPU/128 MiB limits. If `vpn.sensor.asnFile` is set, mount that
operator-maintained `CIDR,ASN,CLASS` file with `extraVolumes` and
`extraVolumeMounts` at the same absolute path.

Kubernetes is the supported backend for A&D or KotH challenges that set
`allowEgress: true`. rsctf installs the workload's NetworkPolicy before its Pod
exists, permits public Internet destinations plus cluster DNS while excluding
private and link-local ranges, and keeps service ingress scoped to the
competition network. Docker fails closed for the same setting because a shared
external bridge cannot provide equivalent per-workload isolation.

## Scale with runtime roles

Use one Helm release per role so each can scale independently.
The supported topologies are one `all` release, `web` plus one `control`, or
`web` plus `engine` workers and one `network` owner. Run a `migrate` release
before starting a fresh split installation. For upgrades, never run that Job
under old serving Pods: use the enforced
[maintenance cutover](./operations.md#update-helm), which scales every named
runtime release to zero, waits for Pod termination and Job success, and restores
only the new immutable digest. Schema rollback requires restoring the matching
database backup; do not roll a runtime back to an old image after migration.

All split releases must disable the bundled PostgreSQL and Redis, use their one
external/shared database and Redis URL through a pre-created Secret, pin the
same non-latest image tag, name the same externally pre-created challenge
namespace with `createChallengeNamespace: false`, and use the same RWX
claim/storage configuration. The chart rejects a split release that violates
those lifecycle boundaries. Only `web` and `engine` scale above one. Do not run
`control` alongside `engine`/`network`.

See [Scale the single binary](./scaling) for complete values, portable stateful-routing
examples, database pool budgeting, and graceful draining.

## Use external PostgreSQL, Redis, or S3

For production, use a managed or independently backed-up PostgreSQL service and
provide its URL through a Kubernetes Secret. Redis is mandatory for split
roles: it supplies shared cache, realtime event fanout, maintenance election,
and distributed API rate limiting.

Set `storage.backend: s3` to use an S3-compatible bucket for
content-addressed blobs, preferably with `storage.s3.existingSecret`. Keep the
files PVC too: repository worktrees, packet captures, checker material, and
snapshots still use `persistence.mountPath`. A multi-node role topology normally
needs `ReadWriteMany`, not the chart's single-replica `ReadWriteOnce` default.

## Remove the release

```bash
helm uninstall rsctf --namespace rsctf-system
```

Inspect PVCs, Secrets, and the challenge namespace before deleting them. Removing the challenge namespace deletes every dynamically created challenge Pod, Service, and NetworkPolicy in it.
