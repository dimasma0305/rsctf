# Per-challenge runtime profiles

An administrator configures named runtimes. Challenge managers select a profile
in the challenge editor or `container.runtimeProfile` in `challenge.yml`.
Existing installations keep the deployment default when no profile is selected.
Keep `RSCTF_CONTAINER_BACKEND=docker` for Docker as the default. Kubernetes and
MicroVM are explicit opt-ins; the bundled challenge examples leave their profile
options commented out.

```json
[
  {"id":"docker","name":"Docker","backend":"docker","isolation":"container"},
  {"id":"kubernetes","name":"Kubernetes","backend":"kubernetes","isolation":"container"},
  {"id":"microvm","name":"MicroVM","backend":"kubernetes","isolation":"microvm","runtimeClass":"kata-qemu-runtime-rs"}
]
```

Set this JSON as `RSCTF_RUNTIME_PROFILES` on every RSCTF runtime role sharing the
database. Helm accepts the same objects in `runtimeProfiles`. The limit is 16
profiles and 16 KiB of configuration. IDs are lowercase letters, digits and
hyphens, at most 40 characters; `default` is reserved. Names can be changed.

Docker profiles use the configured local Docker daemon. Kubernetes profiles use
the configured kubeconfig/in-cluster credentials and challenge namespace. These
profiles do not provision a cluster or install a hypervisor. A Docker deployment
using Kubernetes profiles needs credentials and network access to that cluster;
Compose does not mount a kubeconfig automatically. Docker capacity reservations
remain shared across profiles on the same installation/daemon.
Set `RSCTF_K8S_AD_SERVICE_CIDR` for any Kubernetes profile and ensure the checker
and proxy roles can route to the cluster's Service addresses. Checker isolation
includes configured profile networks, while each checker receives only its exact
target IP/port permission.

For Helm, Docker profiles require the local Docker backend and its explicit
socket configuration. Kubernetes profiles enable the Kubernetes service account,
challenge namespace and workload RBAC. MicroVM profiles additionally grant `get`
on only their named RuntimeClasses. NetworkPolicy enforcement and a separate
challenge namespace are required just as for the default Kubernetes backend.

## MicroVM prerequisites

MicroVM support uses **Kata Containers through Kubernetes RuntimeClass**. Each
challenge pod runs its OCI image inside a lightweight VM with its own guest
kernel. There is no direct Firecracker API, custom kernel/rootfs upload, or live
VM migration in this integration.

Install Kata on dedicated eligible nodes following the upstream
[installation guide](https://kata-containers.github.io/kata-containers/installation/).
Nodes need hardware virtualization (or nested virtualization), working KVM and
the Kata networking/VSOCK prerequisites. Select a RuntimeClass whose handler is
`kata` or starts with `kata-`; it must declare positive `overhead.podFixed.memory`
so the scheduler accounts for VM overhead. Configure node selection/tolerations
on the RuntimeClass so pods reach nodes that actually have that runtime.

RSCTF checks the class at startup and again before creation. Missing classes,
ordinary container handlers and absent VM memory overhead are errors. It never
retries a MicroVM request with an ordinary container runtime. RuntimeClass
metadata is administrator policy, not proof of virtualization: run the real
acceptance test and verify the guest kernel on your cluster before enabling it
for players.

## Switching a challenge

1. Build or pull a verified immutable image. Use a portable `repository@sha256:…`
   image to move between Docker and Kubernetes/Kata. A Docker-local `sha256:…`
   image cannot move to a different runtime. Publish archive-built images to a
   registry before selecting Kubernetes/Kata.
2. Select the destination runtime profile and save. New instances use it;
   existing instances keep their original backend, filesystem and endpoint.
3. To move running instances, choose **Save and replace instances** and confirm.
   This stops the challenge's current instances, including test instances, using
   their original owners. Writable files and player changes are lost. Jeopardy
   players start fresh instances; shared/A&D/KotH services are recreated by their
   normal provisioning loop. Scores and submissions remain in PostgreSQL.

Replacement uses the existing durable definition transition and revision check.
An incompatible image or stale editor fails before teardown. A cleanup failure
leaves the recoverable transition fenced; retry the same operation rather than
changing profile configuration underneath it. Change an image and finish its
build before requesting replacement.

The saved selection also round-trips through cloning, game archives and repository
push-back. Archive import requires the named profile to exist on the destination:

```yaml
container:
  # Omit runtimeProfile to preserve the existing selection during repository sync.
  # New challenges use the deployment default when omitted.
  # runtimeProfile: microvm
  # runtimeProfile: null  # explicitly return to deployment default
  containerImage: registry.example/ctf/service@sha256:<64-hex-digest>
```

Repository sync preserves enabled runtimes and reports deferred runtime changes;
use the editor's explicit replacement workflow for a running challenge.

## Boundaries and retirement

- A&D/KotH runtime changes are locked after official scoring starts. Rehearse and
  select the runtime before the competition. With event VPN enabled, the selected
  backend must match the deployment's VPN routing backend.
- Host packet capture, interactive Docker inspectors and Docker filesystem
  exports retain their backend limitations. Disable capture before selecting
  Kubernetes/Kata. MicroVM isolation does not make those Docker tools portable.
- Multi-service trusted-worker definitions, participant BYOC and standalone
  exercise instances retain their existing runtime configuration. Profiles apply
  to platform-hosted single-container game challenges.
- Profile IDs, backend, isolation and RuntimeClass are registered permanently in
  the database. To retire one, keep its original definition and set
  `"enabled":false`. New launches are rejected; old instances and crash orphans
  remain manageable. Removing or repurposing a registered profile stops startup.
  Retain access to its original daemon/cluster and keep all replicas configured
  consistently. Database rollback does not restore deleted instance files.

## Verification

Unit/wire regressions exercise immutable routing, disabled profiles, bounded
operation keys, YAML and Kata class validation. PostgreSQL tests cover registered
profile identity, authorization, revision conflicts, preservation of existing
instances and scoring history, and recovery after interrupted replacement. Real lifecycle tests in
`src/services/container/profiles/real_tests.rs` use a pinned nginx-unprivileged
image and verify HTTP readiness, retry adoption, flag injection, old-owner
cleanup and fresh writable files. The full six-direction switch sequence also
requires a Kata guest kernel different from the Docker host kernel. Mocked
Kubernetes tests alone do not satisfy that gate.
