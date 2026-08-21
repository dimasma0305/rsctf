# Security design decisions and accepted risks

This note records intentional rsctf behavior that may look suspicious during a
security review. Under the current trust model, the behaviors below are product
features or documented deployment tradeoffs, not vulnerabilities by themselves.
Do not file or change them solely because a scanner or audit identifies the
underlying capability.

This is not an exemption for related implementation bugs. Reopen a finding when
the behavior crosses the stated trust boundary, bypasses the selected mode, or
contradicts the conditions recorded below. Changes to the trust model must update
this note deliberately.

## 1. Direct dynamic-challenge ports

`ContainerPortMappingType=Default` intentionally publishes dynamic Jeopardy
challenge ports directly. Docker may bind a daemon-selected port to the public
entry, while Kubernetes may expose a NodePort. Per-connection rsctf authorization
and traffic capture are features of the optional `PlatformProxy` mode, not promises
made by direct mode.

Operators choosing direct mode are responsible for limiting the published port
range to the intended event audience. The existence of a reachable challenge port,
or the fact that it does not pass through `PlatformProxy`, is therefore not a
vulnerability.

Reopen this decision if a backend publishes a port when proxy-only mode was
selected, advertises an unintended interface, or discloses another participation's
private instance metadata through the rsctf API.

See [Docker deployment](./docker.md) and
`src/controllers/admin/settings.rs::container_port_mapping`.

## 2. Ordinary Jeopardy workload egress

Ordinary Jeopardy challenges may intentionally use outbound networking. The local
Docker backend and the Kubernetes proxy ingress policy preserve that egress. This
is distinct from the stricter A&D, KotH, checker, and BYOC isolation contracts.

Deployments must keep challenge networks away from the rsctf control plane,
PostgreSQL, Redis, cloud metadata, and unrelated private networks as described in
the [security checklist](./security.md). The mere presence of ordinary Jeopardy
egress is not a vulnerability.

Reopen this decision if an A&D, KotH, checker, or explicitly isolated workload can
bypass its deny policy, or if a maintained deployment claims private-network
isolation but does not enforce it.

## 3. Monitor management of packet captures

`Monitor` is a trusted, platform-wide event-operations role. Its intended capture
lifecycle permissions include listing, downloading, inspecting, and deleting
individual or grouped packet captures. The monitor UI and API expose those delete
operations deliberately.

Capture deletion by a current monitor or administrator is not an authorization
vulnerability. Role assignment, revocation, and preservation of evidence needed
for an incident remain organizer responsibilities.

Reopen this decision if a normal user can invoke a monitor operation, a revoked or
demoted monitor retains access, or a future game-scoped monitor can affect a game
outside that scope.

See `src/controllers/game/traffic.rs` and
`web/src/pages/games/[id]/monitor/Traffic.tsx`.

## 4. Secrets stored in PostgreSQL

The rsctf database is a trusted, secret-bearing store. SMTP, registry, OAuth,
CAPTCHA, build-registry, and repository credentials may be stored as plaintext
database values so the application can use them. Responses intended for routine
administration redact retained secret values.

Plaintext storage inside this trusted boundary is an accepted design choice, not
an application vulnerability. Operators must restrict database access and encrypt
database volumes, replicas, exports, and backups.

Reopen this decision if a secret is returned to an unauthorized caller, written to
logs, included in an ordinary export, or exposed outside the database and
administrator trust boundary.

## 5. Author-controlled Markdown, HTML, and CSS

Administrators and game managers are trusted content publishers. Markdown fields
may intentionally contain custom HTML, CSS, forms, media, and external resources
for challenge presentation, posts, notices, writeup instructions, and branding.
Game managers have fewer platform permissions than administrators, but their
published event content is still trusted author content.

The shared sanitizer is intended to remove script elements, event-handler
attributes, and dangerous URL schemes. It is not intended to reduce authored
content to plain Markdown or to prohibit custom presentation. Retained style,
form, and external-resource markup is therefore not a vulnerability under this
author trust model.

Reopen this decision if an ordinary player or unreviewed submission can publish
directly through this renderer, if script-capable markup survives sanitization, or
if authored content gains a platform privilege beyond content presentation.

See `web/src/utils/sanitize.ts` and
`web/src/components/MarkdownRenderer.tsx`.

## 6. Password and login policy

The six-character compatibility minimum, composition checks, per-IP login rate
limit, optional CAPTCHA, and Argon2id verification are the current account-policy
design. A stricter minimum, breached-password screening, MFA, or account-scoped
backoff may be considered as product hardening, but their absence is not treated as
an implementation vulnerability.

Reopen this decision if registration bypasses the configured checks, login bypasses
rate limiting, password verification stops using the intended password hash, or an
account can authenticate without satisfying its admission requirements.

## 7. Remote HTTP attachments

Trusted challenge authors may publish remote attachment links using either HTTP or
HTTPS. rsctf validates and returns the URL but does not fetch it into a trusted
server-side context or claim that a remote attachment has platform-verified
integrity. Transport choice and artifact hosting belong to the author and event
operator.

Accepting an absolute HTTP attachment URL is therefore not an SSRF or application
vulnerability by itself.

Reopen this decision if rsctf begins fetching these URLs, an untrusted user can
replace an approved attachment, or the UI represents a remote artifact as
integrity-verified when it is not.

## 8. Worker bootstrap provenance

The public one-line worker bootstrap intentionally trusts HTTPS GitHub release
assets and verifies the installer or archive against `SHA256SUMS` from the same
release. This keeps the bootstrap independent of a particular GitHub CLI version.
The documented manual installation path provides GitHub attestation verification
for operators requiring an independent provenance check.

The portable bootstrap's co-hosted checksum and Linux `--skip-attestation` path are
accepted supply-chain choices, not a checksum-bypass vulnerability.

Reopen this decision if a checksum mismatch is accepted, downloads can leave the
expected release host without validation, or the bootstrap claims attestation that
it did not perform.

See [worker deployment](./workers.md).

## 9. Docker socket and root-equivalent runtime access

The optional local Docker challenge backend deliberately gives the responsible
rsctf service access to the Docker daemon. That capability is root-equivalent and
is not treated as a sandbox boundary. Maintained documentation requires a
dedicated, trusted host without unrelated workloads or secrets.

The root user and Docker-socket mount in the opt-in Docker deployment are therefore
not privilege-escalation vulnerabilities by themselves.

Reopen this decision if Docker access is enabled without the operator selecting the
backend, is exposed to a player workload, or a maintained deployment claims that
the daemon boundary contains a compromised rsctf process.

## Audit classification

The review that produced this note did not confirm SQL injection, path traversal,
SSRF, command injection, session forgery, unauthenticated remote-code execution, or
script-capable stored XSS. The `rsa` advisory present in the root lockfile was not
reachable from the active PostgreSQL application dependency graph at review time;
it becomes relevant if that optional dependency enters a built target.

These are point-in-time non-findings, not guarantees. Future audits should still
test the implementations while respecting the intentional boundaries above.
