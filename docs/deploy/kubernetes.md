# Kubernetes with Helm

The published rsctf Helm chart deploys the platform, its HTTP Service,
persistent storage, and optional starter PostgreSQL/Redis services. It can also
grant rsctf narrowly scoped permission to create challenge Pods, Services, and
per-instance NetworkPolicies. Every runtime role uses the same image;
`runtimeRole: all` remains the default. You do not need a source checkout.

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

Generate secrets with `openssl rand -hex 32` and protect the file with `chmod 600 rsctf-values.yaml`.

If an ingress controller sets forwarded client-address headers, also set `config.trustedProxyCidrs` to the controller's actual source CIDR. Leave it empty until you know that range; trusting a broad cluster or private network lets other workloads spoof player IPs. See [Reverse proxy and HTTPS](./reverse-proxy).

The bundled starter PostgreSQL/Redis passwords and first-administrator setup
token are generated on first install and retained on upgrades. The Helm notes
print a command that reads the token without placing it in a URL. For
production, use `existingSecret.name` with an external secret manager and an
external PostgreSQL service instead of keeping sensitive values in Helm release
data; include the configured `bootstrap-token` key.

## Install

```bash
helm upgrade --install rsctf oci://ghcr.io/dimasma0305/charts/rsctf \
  --version "$RSCTF_VERSION" \
  --namespace rsctf-system \
  --create-namespace \
  --values rsctf-values.yaml \
  --wait
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
- Normal challenge Services use random NodePorts.
- `RSCTF_K8S_PUBLIC_ENTRY` must lead players to nodes where those ports are reachable.
- A&D services use ClusterIP and per-instance NetworkPolicies.
- KotH marker reads use the narrowly scoped `pods/exec` subresource.
- Private challenge image pull credentials are not currently attached to generated Pods.
- Challenge images must be portable repository digests. Configure
  `registry/name@sha256:...` directly unless a separate Docker-enabled build
  role resolves the mutable input tag; daemon-local archive builds cannot run on
  Kubernetes nodes.

The chart creates a ServiceAccount and a namespaced Role for only the resources the current backend uses.

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

Managed clusters with restricted Pod Security may reject this mode. A NetworkPolicy-capable CNI is necessary but not sufficient; verify routing and isolation with two real test teams.

Kubernetes is the supported backend for A&D or KotH challenges that set
`allowEgress: true`. rsctf installs the workload's NetworkPolicy before its Pod
exists, permits public Internet destinations plus cluster DNS while excluding
private and link-local ranges, and keeps service ingress scoped to the
competition network. Docker fails closed for the same setting because a shared
external bridge cannot provide equivalent per-workload isolation.

## Scale with runtime roles

Use one Helm release per role so each can scale and roll back independently.
The supported topologies are one `all` release, `web` plus one `control`, or
`web` plus `engine` workers and one `network` owner. Run a `migrate` release
before upgrading the long-running roles.

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
