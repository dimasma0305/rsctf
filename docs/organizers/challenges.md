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

To register files that teams edit over SSH, include `.rsctf-ssh.json` at the
root of the organizer's starter ZIP before kickoff. List each source file
beside its location in the running container. For example, a Dockerfile that
copies `src/exercise.py` to `/app/exercise.py` uses this entry:

```json
[
  { "sourcePath": "src/exercise.py", "containerPath": "/app/exercise.py" }
]
```

After saving, expand **Registered SSH source files** to check the paths.
Registration is optional and accepts up to 32 regular files from the starter
ZIP. The list is pinned at kickoff. This setting records the organizer's
choice of paths. Open **SSH edit history** on the team's challenge or in the
admin A&D operations table to see which registered files changed. RSCTF samples
the service when a hosted Docker shell opens, once per minute while it is open,
and when it closes. Multiple saves between samples appear as one change.

The private history retains 128 observations per team service. Unchanged files
update the last-seen time. Each observation stores Git content hashes and
executable bits. An unreadable or oversized file (over 240 KiB) marks the
sample unavailable. Docker also reports files on a tmpfs mount as unavailable.
In **Team revision history**, open a saved ZIP revision and choose **Compare ZIP
with last SSH report**. The comparison checks file contents and executable bits
for the registered paths. “Added” means the ZIP contains a file that was missing
at the last sample. “Deleted” means the file was observed over SSH and is missing
from the ZIP. The report time shows when RSCTF last sampled those files.

To let teams recover edits after a hosted Docker container is replaced, add
`"retainContent": true` to each source entry whose contents the team may keep.
With the default setting, the history records only hashes and file modes.
The image's shell user must be able to read every selected file and run
`head`, `timeout`, and `base64`. A missing file records a deletion.
In **SSH edit history**, the team can download its eight most recent recoverable
observations during the game. Each ZIP starts from the organizer's starter and
applies the selected files and deletions. Other files come from the starter.
Review the ZIP and submit it through the regular patch workflow to deploy it.

When you save a starter before the game, the editor also records the challenge's
successful build by its immutable image reference. If you upload the ZIP first,
the editor shows **Build the challenge image** until the first successful build
before kickoff. Later image rebuilds keep the recorded original. Replacing
the starter ZIP before kickoff pairs the new source with the current build.
For older starters and ZIPs first uploaded during play, the editor shows when
the original image is unknown.

If self-reset is enabled, a player's **Reset** rebuilds their service from the
recorded original image. When no original was recorded, it uses the challenge's
current successful build. Reset discards edits inside the container and incurs
the usual SLA cost during the rebuild.

RSCTF's Docker cleanup keeps the recorded local image until the game ends.
All teams share that image. Saving a starter adds one database reference per
challenge. For registry images, retain the manifest and layers at the recorded
digest in your registry for the duration of the event.

During the game, an accepted teammate can open **A&D Toolkit → Patch your
service**, choose their hosted challenge, and use **Download starter ZIP**.
After editing, they can choose their ZIP and click
**Compare files** to see additions, modifications, and deletions. Preview reads
the submitted ZIP in memory. By default it compares against the team's latest
private revision, or the starter ZIP for a team's first save. Choose **Starter
ZIP** under **Compare with** to see all changes since the original. If a
teammate saves while a latest-revision preview is uploading, compare again
to see the new baseline before saving.

After comparing, a teammate can choose **Save private revision**. Open **Team
revision history** to see the time and Git commit ID of each saved ZIP. Select
a revision to see which files changed since the previous save. Choose **Download
revision N ZIP** to recover that version, including executable permissions. Saving
records source only. The service continues running its current version.
RSCTF shares the starter across teams and stores each additional file's
contents once per team service, even when the file is renamed. Each service
can keep up to 4096 revisions and 512 MiB of source blobs and manifests.

To share deployment counts, enable **Share patch activity** in the A&D game settings before the event starts. The default is off. For each hosted ZIP challenge, players can see how many deployments took effect in a round once another round finishes. Pausing the scheduler also pauses publication.

To share a file’s change history after the competition, add `"shareAfterClose": true` to its entry in the organizer’s `.rsctf-ssh.json` before kickoff. With **Share patch activity** enabled, accepted teams in the same division can open the archived challenge to see the deploying team, revision number, and which marked starter paths were modified or deleted. Each comparison uses the original starter. The archive opens after the game ends, all rounds are final, and practice mode is off. File contents, team-added filenames, SSH observations and BYOC reports stay private.

Run `rsctf challenge patch-preview current.zip candidate.zip` to review the
files added, modified, or deleted in a candidate patch. Both ZIPs need a
`Dockerfile` at the root. The command runs locally and prints JSON with the
two Git SHA-256 tree IDs and the changed paths. It reads one archive at a time
and does not run the Dockerfile. A change to the executable bit is a modification.
Identical tree IDs mean the two archives supply identical source inputs. When
reusing a built image, also check the base image digest and external dependencies.

To prepare a saved revision for an isolated builder, download its ZIP and use
the full `treeId` from that revision in the team's patch history API. Run
`rsctf challenge patch-context revision.zip "$tree_id" > context.tar`.
Use the tar only if the command succeeds. It contains the reviewed files and
their executable bits. A damaged ZIP or a different revision fails before any
source is written. The command only prepares the input for a build.

### Build a revision on an offline worker

On a Linux x86-64 worker, build the guest kernel and initramfs once. The
packaging step needs Docker Buildx and network access to fetch the guest tools.

```sh
docker buildx build --platform linux/amd64 --output type=local,dest=patch-guest scripts/ad-patch-builder
```

For each revision, run the following command as an unprivileged worker
account with Python 3, QEMU (`qemu-system-x86_64` with seccomp support), and
the RSCTF CLI on its path. Use the saved revision's full `treeId`.

```sh
python3 scripts/ad-patch-builder/build.py revision.zip "$tree_id" \
  --guest patch-guest --output candidate.oci.tar
```

Each job gets a fresh VM with two virtual CPUs, 1.5 GiB RAM, and only a
loopback network interface. The worker allows five minutes for emulation and a 256 MiB OCI
archive. Add `--kvm` on workers with KVM access. A successful command prints
the archive's SHA-256 and creates an owner-readable file at the requested path.
The archive contains an OCI index and a Docker load manifest. Both reference
the same layer files, so compatibility adds only manifest metadata.
Use a new path for each candidate. The running team service keeps its current
image throughout the build. Use a local output filesystem that supports Linux
unnamed temporary files (`O_TMPFILE`), such as ext4 or XFS. Interrupted jobs
release their scratch files when the worker exits.

A self-contained Dockerfile can use `FROM scratch`. For an application
with dependencies, prepare an approved Linux amd64 OCI image containing them,
use `FROM rsctf-starter` in the challenge Dockerfile, and pass
`--base-oci starter.oci.tar --base-manifest "sha256:$manifest_digest"`.
The digest identifies the manifest **inside** the OCI archive. The guest
reads this base from a separate read-only disk. Package downloads and remote
image pulls require preparing the dependencies before the event.

To accept build requests from the Toolkit, run one worker for the hosted
challenge. Register its starter ZIP first. Set `RSCTF_DATABASE_URL` to the
platform database connection string on the worker host before starting it.

```sh
python3 -m venv patch-worker
patch-worker/bin/pip install -r scripts/ad-patch-builder/requirements.txt
patch-worker/bin/python scripts/ad-patch-builder/worker.py --challenge "$challenge_id" --guest patch-guest
```

The worker uses Python 3.11+ and a direct PostgreSQL session. A worker that uses
a base image takes the same `--base-oci` and `--base-manifest` options as a
local build. Keep its guest files and approved base immutable while it runs.

Players open **Team revision history**, select a revision, and choose **Build
revision**. The button appears when the challenge's worker is connected. Each
team service can queue one build at a time. If a worker stops partway through,
its successor can reclaim the request. The private candidate lives in PostgreSQL
in chunks of up to 1 MiB. Each service retains one successful candidate of up
to 256 MiB, plus its source history. For five teams with five services each,
that caps retained candidate archives at 6.25 GiB. PostgreSQL also needs room
for transactions, WAL, and vacuum. A failed build retains the previous candidate.
When a hosted service publishes an endpoint, RSCTF records the immutable image
used to launch it. Local Docker cleanup retains that image and the team's last
distinct published image until the game ends. Restarting on the same image
preserves the previous version. An interrupted reset keeps its staged image
even if the organizer rebuilds the challenge during recovery. When a hosted
container stops, automatic repair uses the team's last published image and its
current round flag. Before removing a stopped Docker container or resuming an
interrupted reset that still has its old container, RSCTF checks that Docker
can load the selected image. If staging fails or a stored flag prevents the
repair, the old container and its files stay on the host. On resuming a
reset, RSCTF also checks the prepared round
and its official flag before removing the old container. If the round has
ended or its flag is missing, the surviving container keeps its files.
If the saved challenge settings exceed the deployment's current resource
limits, reset and rollback keep the live service online. Adjust the settings
and retry. On Kubernetes, a missing or invalid Kata RuntimeClass also stops
replacement before RSCTF removes the existing Pod.
A stopped service's address is withdrawn so it cannot route
players to a different workload. Before publishing the replacement endpoint,
RSCTF writes the current round's flag to the replacement's
`RSCTF_FLAG_FILE` and reads it back before advertising the endpoint. This also
applies to automatic repairs during a round. Existing scoring receipts keep
their original backend identity. RSCTF records the container ID on the reset job and the service in one
transaction. If that worker stops, its successor can publish the same container
with its files intact. A round change during launch makes RSCTF
discard the stale replacement. Existing services begin recording history at
their next publication. Keep portable digests available in your own registry.

For local Docker services, put team data in image-declared anonymous volumes
at `/data` or `/var/lib/<name>`. An automatic repair reuses those volumes with
the same image. A player reset to the starter or an organizer restart can switch
images when both declare the same set of data volumes on the same Docker runtime.
If the running service has no persistent volume, its ZIP patch must also
declare none. Adding a volume would leave the starter unable to reuse it on rollback.
RSCTF checks compatibility before withdrawing the endpoint or recording reset
downtime. It stops the old writer before starting its replacement and retains
the old container until the endpoint and reset job receipt commit together.
Files in the container's writable layer start fresh. Unverified persistent
mounts leave the reset pending or rejected with the team's data still attached.
The replacement reads the current contents of those volumes, so both image
versions must be able to read the team's stored data format.

For hosted Kubernetes challenges, include a file named `.rsctf-data-dir` in the
root of the organizer's starter ZIP before provisioning. Give it one line with
the directory the service uses, such as `/data` or `/var/lib/rehearsal`.
Each team starts with an empty claim at that directory. Have the service initialize
its files on first use. Reset and automatic repair mount the same team's claim.
The first deployment pins the mount path, the claim and a private Kubernetes
Service. Deploying a ZIP or rolling back creates a new Pod behind that Service.
The team keeps its original ClusterIP and port. Before taking the old service
offline, RSCTF checks that the cluster can pull the selected image using the
team Pod's credentials. The replacement receives the current round's flag
before RSCTF publishes it to the event.

On Kubernetes, reset and automatic repair inspect the existing Pod. If it
has an unregistered persistent volume claim, an `emptyDir`, or another application volume,
RSCTF keeps the Pod so the organizer can recover the team's files. Projected
configuration such as ConfigMaps and Secrets can be recreated.
If retrying a launch finds a stopped Pod with application storage, the error
names the claim or volume. The same check runs if Kubernetes rejects the
Service for that Pod. You can inspect the storage before retrying.

For a hosted challenge with player resets enabled, **Restore previous image**
selects the last different image that actually served that team. The platform
pins the selection in its rollback job and verifies the current service before
retiring it. The starter and previous image actions share one cooldown and
one active replacement per service. Deploying a staged ZIP revision uses
the same cooldown and replacement queue.

On the trusted worker host, an organizer can export a ready build to a new
private file. Use the team's service ID, revision number, and attempt UUID
from its build status. The export checks the archive size and SHA-256 digest
and stops if a newer build replaces the selected one during transfer.

```sh
patch-worker/bin/python scripts/ad-patch-builder/export.py \
  --service "$service_id" --revision "$revision" --attempt "$attempt" --output candidate.oci.tar
```

The command prints the archive digest. Add `--json` to print the archive
digest, the OCI manifest digest for a registry, and the Docker image config
digest together. These identify the exact build attempt that was exported.
For candidates built with an older guest, the last two values are `null`.
When updating an existing builder, migrate the server database first, then
update the worker and rebuild its guest for new jobs.
Exporting leaves the team's running service on its current revision.

To prepare ZIP builds for **Deploy revision** automatically, install `skopeo`
on a trusted host, give its account push access to a private event namespace,
and grant the runtime pull access. Set `RSCTF_DATABASE_URL` on that host.
Run one registry watcher per hosted challenge alongside the build worker:

```sh
patch-worker/bin/python scripts/ad-patch-builder/stage.py \
  --watch-challenge "$challenge_id" \
  --repository registry.example.org/private-event --authfile "$registry_authfile"
```

The watcher checks for eligible builds on startup and waits 30 seconds between
passes. For local Docker, enter the registry's host (including its port) and
read-only account in **Administration → Settings → Pull credentials**. The host
must match the registry in the image digest. Docker also needs to trust its TLS
certificate. The watcher transfers one team's latest ready candidate at a time. A failed
transfer is retried on a later pass while other teams can continue. On restart
it skips builds with a recorded registry digest. Configure a process supervisor
to restart it if the database connection drops. The team chooses when to deploy.

To transfer a single build manually, specify its service, revision and attempt:

```sh
patch-worker/bin/python scripts/ad-patch-builder/stage.py \
  --service "$service_id" --revision "$revision" --attempt "$attempt" \
  --repository registry.example.org/private-event --authfile "$registry_authfile"
```

For a private CA, add `--cert-dir "$registry_cert_dir"`. The command verifies
TLS and the manifest and config digests, then records the immutable reference
under `private-event/service-<id>`. Retrying the same transfer is safe.
Staging leaves the active service running. On Docker with player resets enabled, the
owning team can open **Team revision history**, select the build and choose
**Deploy revision** after its cooldown. The service must already have a
recorded running image and a registered starter image. Before replacing it,
RSCTF rechecks the selected build attempt and its registry digest. If you
change the runtime profile, the current backend must still own the team's data.
Local Docker supports ZIP deployment. Kubernetes supports it for starters
provisioned with a registered data directory and private team Service. Give
the challenge namespace pull access to the registry holding the staged image.

#### Reclaim space in a dedicated patch registry

Patch images are pushed by digest. A registry policy that deletes untagged
manifests can remove a team's active or rollback image. For a self-hosted Docker
Distribution registry dedicated to a single RSCTF patch namespace, use the
following sequence in your registry maintenance job.

Add `--retain-images` to your `stage.py --watch-challenge` or single-build
command to update retention tags as new builds arrive. The watcher verifies
the required tags before publishing each build to players. If tagging fails,
the watcher retries on its next pass. The namespace-wide maintenance pass
also covers builds staged before this option was enabled.

1. Pause staging and changes to hosted services, challenges and patch jobs.
   Keep writers paused until collection finishes. The registry must still accept
   tag writes during the next step.
2. Run the command below against the platform database and require a successful
   exit. Its account needs pull and push access. The host's skopeo policy must
   allow the dedicated patch namespace as a source.
3. Stop the registry or switch all registry replicas to read-only mode. Run
   Distribution's `registry garbage-collect --delete-untagged <registry-config>`
   against that registry's storage. Restore write access and resume RSCTF
   writers after it finishes. Distribution collects across its entire store;
   use this sequence only for a store dedicated to these patch images.

```sh
patch-worker/bin/python scripts/ad-patch-builder/retain.py \
  --repository registry.example.org/private-event --authfile "$registry_authfile"
```

The command reuses `rsctf-retain-*` tags for the current candidate, active
and previous images, organizer images, and images needed by pending or running
patch, reset and rollback jobs. It verifies each manifest by digest and reads
the database references again before reporting success. Reserve those tags for
RSCTF. A repository with live references keeps numbered slots up to its
historical peak of simultaneously referenced manifests, plus one bootstrap tag.
In repositories
whose last database reference has been removed, old retention tags remain until
the organizer retires the event registry. Docker-local image IDs use the
platform's local image retention. Apply `--cert-dir` when using a private CA.

To rehearse with five hosted services, generate a separate hidden event from
the bundled example repository. Each service includes a starter ZIP, two
working patch revisions, a ZIP that fails to build, and a checker. Record the
value of `GET /installation` for each team's service before the first patch.
After activation or rollback, the same value indicates that the team's
SQLite record in `/var/lib/rehearsal` survived the replacement.

```sh
git submodule update --init examples/challenge-repository
python3 examples/challenge-repository/tools/ad-patch-rehearsal/generate.py ../rsctf-rehearsal
```

## BYOC source history

The team runs the setup command shown on its challenge page. For example,
`sh setup-13-50.sh` starts the service and reporting agent for game 13,
challenge 50. Teams using **Bring your own service** save `compose.yaml` in a
folder for that challenge and run `docker compose up -d` there. Both methods
start source history in the background.
The setup script requests the reviewed image revision selected at download
time. If that revision becomes unavailable, a reinstall keeps the team's
existing service and configuration. A fresh script can install a newly
reviewed revision when the Compose file still matches the generated settings.
If a team customizes the generated service definition, setup preserves that
file and leaves the team in control of the running or stopped service.
Teams already running the standalone Compose download
continue managing that stack from their original challenge folder.

- For a fresh installation with a reviewed image whose Dockerfile uses
  `WORKDIR /app`, `/srv/app`, `/opt/app`, `/usr/src/app`, `/var/www/html` or
  `/rsctf-source`, setup shares that directory with the agent read-only.
  Docker seeds a volume from the image once per image revision. Edits inside
  the running service persist across container recreation with the same volume.
  The agent needs read access to registered source files. The BYOC example
  installs its application with `COPY --chown=nobody:nogroup --chmod=0644`
  so a private build checkout also works.
  Allow disk space for one copy of that directory per installed revision.
- The standalone Compose file, placeholder, other image layouts, and existing
  services upgraded from a bundle without a source volume use `./service-src`
  next to the Compose file. Put the service's build context there to observe
  host-side patches. Edits elsewhere in those containers are outside coverage.

The agent checks common source files and build manifests once per minute.
It skips hidden paths, dependency directories such as `node_modules` and
`vendor`, and files or directories mounted separately inside the source tree.
To select exact paths, put a `.rsctf-files` list in the watched directory,
one relative path per line. If that list includes a path on another mount,
the agent reports that the source could not be read. An empty list disables observation.
Existing bundles with an explicit list retain that behavior.
The server collects up to four reports at a time. Connected agents waiting
for a slot are served in order.

Only paths, Git SHA-256 blob IDs and executable permissions are reported.
Contents stay with the team. The agent reuses hashes for unchanged files.
Each scan supports 128 files, 8 MiB per file and 64 MiB total. Discovery visits
up to 2,048 entries and 16 levels of directories. If a limit is exceeded, the
agent reports that the source could not be read. A change reverted between
samples can go unseen.

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
