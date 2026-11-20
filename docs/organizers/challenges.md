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

## Compare two A&D source ZIPs

In the hosted A&D challenge editor, choose **Starter ZIP for team patches**,
select the source archive intended for teams, and click **Save starter ZIP**.
Put the Dockerfile at the root. The editor displays the saved Git tree ID.
You can replace the starter before the game begins. RSCTF keeps one ZIP per
challenge in PostgreSQL, up to 72 MiB, shared across all teams. Saving a starter
prepares the original source for comparison and leaves running services in place.

During the game, an accepted teammate can open their hosted challenge and use
**Download starter ZIP**. After editing, they can choose their ZIP and click
**Compare files** to see additions, modifications, and deletions. Preview reads
the submitted ZIP in memory and compares it with the registered original.

After comparing, a teammate can choose **Save private revision**. Open **Team
revision history** to see the time and Git commit ID of each saved ZIP. Select
a revision to see which files changed since the previous save. Choose **Download
revision N ZIP** to recover that version, including executable permissions. Saving
records source only. The service continues running its current version.
RSCTF shares the starter across teams and stores each additional file's
contents once per team service, even when the file is renamed. Each service
can keep up to 4096 revisions and 512 MiB of source blobs and manifests.

Run `rsctf challenge patch-preview current.zip candidate.zip` to review the
files added, modified, or deleted in a candidate patch. Both ZIPs need a
`Dockerfile` at the root. The command runs locally and prints JSON with the
two Git SHA-256 tree IDs and the changed paths. It reads one archive at a time
and does not run the Dockerfile. A change to the executable bit is a modification.
Identical tree IDs mean the two archives supply identical source inputs. When
reusing a built image, also check the base image digest and external dependencies.

## BYOC source history

A team can register files from the source directory it uses for its service. Put
their relative paths in `.rsctf-files`, one per line. For example, a team can
list `Dockerfile` and `src/app.py`. Enable the read-only `/rsctf-source` mount
in the generated Compose file, point its host side at that directory, and run
`docker compose -f docker-compose.yml up -d rsctf-agent` from the bundle directory.

The agent checks the list every minute and hashes files whose metadata changed.
It reports paths, Git SHA-256 blob IDs, executable permissions, and missing files.
File contents stay with the team. Each service supports up to 128 registered
paths, 8 MiB per file and 64 MiB in total. An unreadable file or unsafe link
makes the sample unavailable. Source history records what the agent observed
between samples. Changes made and reverted inside one interval can go unseen.

Teams can open **Source history** on their BYOC challenge during the game.
Platform admins can open it on the team's service in **A&D Operations**.
RSCTF retains the last 128 distinct reports per service and displays the time
of the latest report. Existing v2 agents continue serving traffic and flags and
show a legacy-agent status. The team controls its deployed image and rollbacks.

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

For repository-managed Jeopardy challenges, select the curve in `challenge.yml`
or `challenge.yaml`:

```yaml
name: Example challenge
type: StaticAttachment
# Uncomment to select Standard, Linear, or Logarithmic explicitly.
# scoreCurve: Standard
minScoreRate: 0.01
difficulty: 5
```

`scoreCurve` accepts exactly `Standard`, `Linear`, or `Logarithmic`. The capitalized
key spelling `ScoreCurve` is also accepted; exports use `scoreCurve`. Invalid
values fail validation instead of being ignored. If the field is omitted or
null, new challenges use `Standard` and existing challenges keep their saved
curve, including selections made in the admin editor. Push-on-edit always
exports the selected curve, including `Standard`.

Set the curve before the event starts. Repository sync preserves locked
Jeopardy scoring once the event starts or the challenge has submission/solve
history; a requested curve change is reported as a deferred grading update.
The curve does not replace A&D or KotH scoring rules.

## Attachments

Upload only the files players need. Remove credentials, source-control history, internal hostnames, and unintended answers. Download the attachment using a normal player account before enabling the challenge.

Local challenge attachments may be up to 192 MiB per upload request. The player download is content-addressed and streamed, and supports a single HTTP byte range so interrupted downloads can resume without loading the complete file into an rsctf replica's memory.

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

For a Docker build from a source ZIP, RSCTF applies the configured tag after
BuildKit finishes and RSCTF verifies the resulting image ID. If the build fails
or the build response is interrupted, the previous tag stays available. You can
retry the build while existing instances continue serving their current image.

Docker images that do not need setuid, Linux capabilities, or writes outside
`/tmp` may opt into the restricted runtime profile with the image label
`org.rsctf.security-profile=restricted-v1`. RSCTF then drops every capability,
enables `no-new-privileges`, mounts the root filesystem read-only, and supplies
a 256 MiB `noexec` tmpfs at `/tmp`. Test the image with this profile before an
event; do not add the label to a pwn or boot2root image whose intended path
depends on those kernel features.

::: warning Kubernetes limitations
Regular Kubernetes challenges use NodePort services. Private registry credentials are not currently injected into generated challenge Pods. Bring Your Own Container (BYOC) and several build, terminal, and snapshot paths remain Docker-only or incomplete. Test your exact mode in the target cluster.
:::

For player-facing Docker challenges, use **Open** networking with
**PlatformProxy**. Docker **Isolated** networking supports exec-only workloads;
Docker does not publish ports on its internal-only bridge. RSCTF rejects that
combination before creating a container and keeps the requested isolation intact.

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

## KotH format

After warmup, official scoring starts when at least one enabled hill has a
successful image build, a provisioned target, and a prepared checker, and the
event's roster and lifecycle requirements are satisfied. RSCTF freezes that ready
hill set. A broken hill no longer holds back the other hills; the operations page
lists excluded hill IDs and their missing prerequisites. Repairing an excluded
hill does not add it to an already-started competition. Prepare every intended
hill before the first scoring boundary if they must all count.

Choose one format before official scoring; RSCTF freezes it with the hill:

- **Container marker** is exclusive boot2root KotH. RSCTF reads
  `/koth/king`, qualifies one holder, and scores acquisition, control, and
  reliability.
- **Leaderboard KotH** is multi-team application or protocol KotH. Every
  eligible team can score in each challenge-native finalized wave. The
  platform-managed target reports completed activity, ordered objective evidence,
  and a Crown only for one unique leader; RSCTF normalizes it and applies the
  constant 95% relative-performance plus 5% Crown formula. An exact top tie has
  no Crown.

Leaderboard mode is not merely another marker transport. It has no single holder,
provisional crown, or champion-cooldown score. It still uses pristine lifecycle
resets, exact capability generations, an independent functional checker, and
field-wide voids for platform failures.

Configure Leaderboard reporting from the KotH operations table before official scoring.
Set the private managed-reporter origin on the lifecycle-owning rsctf role. See
[Managed Leaderboard KotH reporting](./koth-api-observer) for evidence design,
normalization, anti-cheat rules, the wire contract, and the runnable example.

## Review and enable

Keep new challenges disabled until another organizer reviews the description, downloadable files, flag, scoring, and runtime. Enable them only after testing with a non-admin account.

## Import from GitHub

Repository bindings can import events and challenges from Git. A bound repository needs a `.gzevent`; standalone `challenge.yaml` files are not imported. A rescan preserves operator-edited game settings and updates challenges in place by binding-relative manifest path, retaining challenge IDs and solve/scoring history. Missing played manifests are retained as disabled tombstones or rejected while event state still depends on them; they are never silently cascade-deleted.

Hints saved in the challenge editor or imported from `challenge.yaml` are private drafts. Use **Release hint** in the challenge editor to publish them to players in order. Use **Unrelease hint** on the latest released hint to return it to draft; repeat from the end to retract more than one. Appending a draft preserves the released prefix, while editing or reordering a released hint retracts that hint and every later hint until an organizer releases them again. Hints on already-enabled challenges remain released when upgrading from a version without explicit hint publication; hints on staged, disabled challenges remain drafts.

Private repositories use a PAT stored in PostgreSQL, so database backups are sensitive. Push-on-edit needs a writable branch and write-capable token. Automatic interval scans are not currently scheduled; use **Scan now** after repository changes.

A checker may include `requirements.txt` beside `run.py`, but every entry must
be a simple, exact PyPI pin. rsctf installs accepted packages wheel-only and
rejects URLs, local paths, pip options, editable installs, version ranges, and
source builds. Review the repository commit and dependency pins before scanning:
checker preparation is a trusted administrator action and needs outbound PyPI
access from the rsctf process performing it.

See [GitHub integration](../deploy/github#challenge-repository-bindings) for the operational model.

For a complete working layout, import the [sample challenge repository](./sample-repository), which contains all six challenge types and explains the current Dynamic Attachment/Kubernetes limitations.
