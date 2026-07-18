# Security checklist

Use this checklist before exposing rsctf to untrusted players.

## Account and browser boundary

- Register the first administrator immediately and use a unique password.
- Disable open registration when it is not needed.
- Serve one canonical `https://` origin and set `RSCTF_PUBLIC_URL` exactly.
- Keep secure cookies enabled in production.
- Configure OAuth, SMTP, and CAPTCHA secrets through the documented environment variables.
- Restrict administrator, monitor, and game-manager roles to the minimum necessary people.

## Secrets

- Generate a unique JWT secret with at least 32 random bytes.
- Keep `.env`, Kubernetes Secret material, repository PATs, OAuth credentials, SMTP passwords, and registry credentials out of Git.
- Encrypt backups; PostgreSQL can contain repository tokens and admin-configured secrets in plaintext.
- Rotate leaked credentials and revoke affected sessions/tokens.
- Never paste a live flag, VPN profile, A&D Bearer token, or private key into a public issue.

## Network

- Expose only the ports the selected deployment uses.
- Trust forwarded IP headers only from the immediate proxy or a proxy-only network.
- Firewall dynamic challenge ports to the intended audience.
- Keep PostgreSQL and Redis off the public Internet.
- Use a dedicated host/VM or cluster for untrusted challenge workloads.
- Verify that Docker/Kubernetes network ranges do not overlap your LAN, VPN, or cloud routes.

## Dynamic challenges

- Treat a mounted Docker socket as host-root access.
- Pin reviewed challenge images or digests; avoid mutable production tags.
- Enforce CPU and memory limits.
- Test that a challenge cannot reach the rsctf control plane, database, Redis, or cloud metadata.
- Use a CNI that actually enforces NetworkPolicy in Kubernetes.
- Do not run mutually untrusted games on a shared A&D bridge without an accepted isolation plan.

## Repository and checker supply chain

- Treat **Scan now** and checker approval as trusted administration actions.
  Review the exact repository commit first: a scan can build checked-in
  Dockerfiles and prepare executable checker code.
- Review every checker `requirements.txt`. rsctf accepts only simple, exact
  PyPI pins and installs packages wheel-only, rejecting URLs, paths, pip
  options, version ranges, and source builds. Those restrictions remove several
  installation paths but do not make a compromised or malicious wheel safe.
- Allow the rsctf process performing a scan or approval to reach PyPI and its
  package file hosts only when checker dependencies are needed. Package
  resolution happens before the runtime checker sandbox and fails closed when a
  compatible wheel cannot be downloaded.
- Restrict repository-binding and challenge-approval permissions to trusted
  administrators, and review dependency changes with the same care as checker
  source changes.

## A&D and Bring Your Own Container (BYOC)

- Run only one VPN kernel owner (`all`, `control`, or `network`) with TUN access.
  Every round-engine role also needs `NET_ADMIN` for the process checker's
  uid-scoped, default-deny egress firewall; `web` receives neither capability.
- Treat a round-engine startup failure to install that firewall as a deployment
  error. Keep `NET_ADMIN` for the role's lifetime: rsctf intentionally refuses
  to run a checker unless it can add and remove that UID's exact target rule.
  The role also needs `CHOWN`, `SETUID`, `SETGID`, and `KILL` so the parent can
  create the per-run scratch area, drop the child identity, and reap that child
  after a timeout. The bundled manifests drop the default capability set and
  add only those four, `NET_ADMIN`, and (for a capture owner) `NET_RAW`.
- Run checker-owning roles on Linux with Landlock ABI v3 available and enabled
  in the active LSM set (`CONFIG_SECURITY_LANDLOCK`), seccomp filter support
  (`CONFIG_SECCOMP` and `CONFIG_SECCOMP_FILTER`), and the netfilter owner match
  used by the OUTPUT chain. At startup rsctf sends a no-network canary through
  the real re-exec launcher and proves the UID/GID drop, `no_new_privs`, active
  seccomp filter, Landlock denial, and allowed scratch write. A failed proof
  exits before the role publishes a heartbeat or becomes ready.
- Reserve the numeric UID interval beginning at `RSCTF_CHECKER_UID_BASE` for
  `RSCTF_CHECKER_PROCESS_BUDGET` identities inside each checker Pod/container.
  Do not assign that interval to rsctf or another daemon. Each live checker gets
  a distinct UID; the same budget bounds custom-checker roots, while the Pod or
  container PID limit remains the aggregate backstop.
- Checker subprocesses have no DNS egress. The trusted parent resolves a
  service hostname once and passes a literal IP through `RSCTF_TARGET_IP`; this
  prevents a checker from tunneling flags through a recursive resolver.
- Each leased checker UID receives one temporary TCP rule for that exact
  resolved service IP and port. It cannot contact another team peer or service;
  rule cleanup completes before the UID returns to the pool, and a cleanup
  failure poisons that UID until restart.
- With the Kubernetes backend, configure the authoritative cluster Service
  CIDR as `RSCTF_K8S_AD_SERVICE_CIDR` on every runtime role, even without VPN.
  Web provisioning needs it to build policy, and checker roles use it as the
  parent-side target allowlist; a built-in TCP probe cannot defer this startup
  validation.
- Protect WireGuard profiles and rotate access after roster changes.
- Confirm checker and team routes cannot reach control-plane interfaces.
- Keep BYOC Docker-socket access opt-in and recommend disposable team hosts.
- Monitor the checker duration, submission rate, VPN peer changes, and unusual service egress.

## Operations

- Back up and restore-test PostgreSQL and uploaded files.
- Keep the host, cluster, Docker, ingress controller, database, and rsctf image updated.
- Review logs and anti-cheat results during events.
- Preserve an incident timeline in UTC.
- Remove temporary roles, tokens, rules, and exposed ports after the event.

Treat this guide as a deployment baseline, then maintain a threat model and
incident-response plan for your own infrastructure and event.
