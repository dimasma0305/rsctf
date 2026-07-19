# Create challenges

Choose the simplest challenge type that delivers the intended experience. Every dynamic runtime increases infrastructure and recovery work during the event.

## Challenge types

| Type | Use it when | Infrastructure |
| --- | --- | --- |
| Static attachment | Every team downloads the same files | File storage only |
| Dynamic attachment | Downloads or flags differ per team | File storage and generated data |
| Static container | Players share or directly access one service | Pre-provisioned service |
| Dynamic container | Each participant/team needs an isolated instance | Docker or Kubernetes backend |
| Attack & Defense | Every team defends a service and attacks targets | Container backend, checker, and event networking |
| King of the Hill | Teams compete for a shared controlled service | Container backend and working control checks |

## Required content

Write the description for someone who has not seen the challenge source. State:

- The goal and flag format
- What files or endpoint are provided
- Whether destructive actions are expected
- Network scope and prohibited targets
- Any unusual client/tool requirement
- What evidence to send privately if the challenge fails

Do not put deployment secrets, checker credentials, or real flags in the player-visible description.

## Flags and scoring

Add at least one valid flag or configure the dynamic flag behavior required by the challenge. Test exact casing and whitespace.

Choose an initial score, minimum score, and decay method. rsctf supports standard exponential, linear, and logarithmic decay, plus first/second/third-solve bonuses. Simulate expected solve counts so the final value is not surprising.

## Attachments

Upload only the files players need. Remove credentials, source-control history, internal hostnames, and unintended answers. Download the attachment using a normal player account before enabling the challenge.

## Container challenges

Configure the image, exposed port, memory, CPU, and lifetime. Then:

1. Build or pull the configured image and confirm rsctf reports success; rsctf
   records the resolved immutable digest used at runtime.
2. Launch a test instance through rsctf.
3. Connect through the player-visible endpoint.
4. Verify flag delivery and restart behavior.
5. Destroy the instance and confirm cleanup.

Prefer a repository digest in the challenge definition. A mutable tag may be
used as build input on a Docker-backed installation, but rsctf resolves and
provisions its immutable digest. Changing the tag clears that pin and requires a
new successful build/pull.

::: warning Kubernetes limitations
Regular Kubernetes challenges use NodePort services. Private registry credentials are not currently injected into generated challenge Pods. Bring Your Own Container (BYOC) and several build, terminal, and snapshot paths remain Docker-only or incomplete. Test your exact mode in the target cluster.
:::

## A&D challenge checks

In addition to the container test, verify:

- Each focused check is deterministic, read-only, and order-independent; the
  whole shuffled suite completes well inside one tick.
- Flag planting and retrieval work after a service restart.
- `allowEgress` matches the deployment backend: keep it `false` for Docker,
  which fails closed, or use Kubernetes when the service genuinely needs
  per-workload isolated outbound access.
- Self-reset and SSH behavior match the published rules.
- Two teams can reach each other only through the intended path.
- A failed service produces the expected availability result.
- The registered A&D check suite collectively verifies complete service health
  and the current flag. Every function is attempted once in shuffled order.
- A failed function does not skip the remaining suite; verify aggregate verdict
  priority is InternalError, Offline, Mumble, then OK.

Shuffling checker request order or varying fingerprints is useful
defense-in-depth against brittle checker detection, but it does not hide a
stable checker source IP. Do not treat the shuffled suite as a complete
Superman-defense mitigation or claim that it prevents source-IP allowlisting.
Pair it with clear event rules, network design, monitoring, and player-visible
functional checks. The legacy `@ad_checker` / `@koth_checker` single-function
API remains valid when variation is unnecessary.

## Review and enable

Keep new challenges disabled until another organizer reviews the description, downloadable files, flag, scoring, and runtime. Enable them only after testing with a non-admin account.

## Import from GitHub

Repository bindings can import events and challenges from Git. A bound repository needs a `.gzevent`; standalone `challenge.yaml` files are not imported. A rescan preserves operator-edited game settings but recreates that game's challenges from repository content.

Private repositories use a PAT stored in PostgreSQL, so database backups are sensitive. Push-on-edit needs a writable branch and write-capable token. Automatic interval scans are not currently scheduled; use **Scan now** after repository changes.

A checker may include `requirements.txt` beside `run.py`, but every entry must
be a simple, exact PyPI pin. rsctf installs accepted packages wheel-only and
rejects URLs, local paths, pip options, editable installs, version ranges, and
source builds. Review the repository commit and dependency pins before scanning:
checker preparation is a trusted administrator action and needs outbound PyPI
access from the rsctf process performing it.

See [GitHub integration](../deploy/github#challenge-repository-bindings) for the operational model.

For a complete working layout, import the [sample challenge repository](./sample-repository), which contains all six challenge types and explains the current Dynamic Attachment/Kubernetes limitations.
