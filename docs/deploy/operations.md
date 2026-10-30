# Back up and update

## Simple commands

For a basic wizard installation (one server, local uploaded files, no challenge
containers or VPN), open a terminal in the installed `rsctf` folder:

```bash
./rsctf update                 # verifies the release, backs up, then updates
./rsctf backup                 # saves accounts, games, files and settings
./rsctf backups enable        # daily at 03:00 in the host timezone; keeps 7
./rsctf backups disable       # keeps existing backups
./rsctf status
./rsctf doctor
```

These commands need Python 3.9 or later and Docker access. Scheduling also needs
cron. Use `./rsctf backups enable --time 05:30` to choose a quiet time. Backups
briefly pause the game so the database and uploaded files agree. Check
`backup.log` after a scheduled run. Manual backups keep all copies unless you
pass `--keep 7`. Store another copy off the server; backups contain secret settings
and are saved with owner-only directory permissions. Failed partial copies are
never treated as completed backups.

To recover, copy the backup's printed folder path and choose a new folder:

```bash
./rsctf restore backups/YOUR-BACKUP --to ../recovered-rsctf
```

The command verifies every saved file, downloads the matching verified release,
and restores a separate copy at `http://localhost:8081`. It keeps your current
installation untouched. Use `--port 8082` if that port is taken. Sign in with
your existing account, check a game and an uploaded file, then configure the
recovered copy's public address when ready. Never restore an untrusted backup.
If recovery fails, its separate project is stopped and retained for inspection;
fix the reported problem and choose another new destination folder.

If an update is interrupted, rerun `./rsctf update`. It retains the selected
release and resumes the migration cutover. It never downgrades a migrated
database. If a backup was interrupted, check `./rsctf status` before resuming
the same application image. Advanced topologies, Docker challenges, VPN and
S3 storage use the procedures below.

An rsctf backup is complete only when it contains PostgreSQL and the uploaded-file storage from the same operational period. For a single ended event, the per-event archive described in [Back up and restore competition data](../organizers/games#back-up-and-restore-competition-data) complements, but does not replace, this platform backup.

## Docker backup

Stop application writers before taking **either** snapshot, and keep them stopped
until both the database dump and file/object snapshot are complete. This includes
web, control, engine and network roles, scheduled imports, and any external writer
to the shared storage. Taking a live database dump first and stopping writes only
for the later file copy can leave restored references pointing at deleted files.
Keep PostgreSQL running to produce its logical dump.

Create a PostgreSQL dump from the managed Compose stack:

```bash
mkdir -p backups
cd deploy
docker compose exec -T db \
  pg_dump -U rsctf -d rsctf -Fc \
  > "../backups/rsctf-$(date -u +%Y%m%dT%H%M%SZ).dump"
```

While writers remain stopped, archive the named file volume with a temporary
container; determine the actual volume name with:

```bash
docker volume ls --filter label=com.docker.compose.project=rsctf
docker compose config --volumes
```

Keep `deploy/.env` in an encrypted secret backup. Without its JWT secret and
database password, recovery becomes more disruptive; without the stable
identity hash key, privacy-preserving anti-cheat observations cannot be
correlated across the restore. Never commit it to Git.

## Kubernetes backup

Use the backup process provided by your PostgreSQL operator or managed database. Snapshot/export the PVC mounted at `/data/files`, and preserve the rsctf Secret in an encrypted secrets system. Test restoration into a separate namespace.

## Test a restore

A backup you have never restored is only a hope. On an isolated installation:

1. Restore PostgreSQL.
2. Restore the file volume/PVC.
3. Restore the same configuration secrets.
4. Start the matching rsctf version.
5. Verify accounts, one game, attachments, writeups, and a score.

The disposable regression in `tests/load/restore-drill.mjs` automates this local
storage path. It creates its own databases, migrates and seeds an event with an
accepted solve and writeup, shuts down its writer, dumps PostgreSQL and copies
files/configuration, removes the originals, and restores into a new database.
It compares all public table rows, validates blob hashes and sizes, checks mutable
working files, and exercises authenticated downloads and the scoreboard through
the restored application. Missing/corrupt blobs and incorrectly signed sessions
must fail. CI runs it against the same tested server artifact and retains timing
and source/binary hashes.

The S3 variant additionally backs up referenced objects, deletes the original
objects and restores into a fresh prefix, then reads them through RSCTF's S3
adapter. It uses an isolated loopback RustFS fixture and checks the same corruption
and reference boundaries. It does not establish multi-host or object-version-history
recovery guarantees. For production S3, preserve
the referenced object versions and shared mutable working storage together with
the database snapshot; restoring only the POSIX volume is insufficient. Do not
resume writes until restored references have been checked against object inventory.

## Extending an ended event

An event extension and admission of ended-workload cleanup take the same database
row lock. If the extension commits first, cleanup skips the newly active event.
Once cleanup has claimed a service or target, extending the event returns HTTP
409 until cleanup completes. Its durable claim remains through a failed attempt,
executor restart, or expired lease: an expired lease alone does not prove that
snapshot capture or container destruction has stopped.

Keep an eligible maintenance consumer running (`all`, `control`, or `engine`) and
resolve the snapshot/runtime error reported in its logs. Retry the same editor
operation after cleanup recovery finishes; successful retries are idempotent.
Do not manually clear cleanup tokens or runtime identifiers to bypass this
conflict, because doing so can allow an old teardown to remove a resumed workload.

Retained A&D snapshots are available only when they represent the current event
end and, while a runtime is present, that exact container. An extension or runtime
replacement can therefore hide the previous final snapshot. Cleanup preserves
its stored reference until a valid replacement commits; a failed or cancelled
upload does not discard it. The new snapshot remains downloadable after the
runtime is destroyed, subject to the configured retention and access policy.

## Update Docker Compose

Back up first and export the reviewed immutable image digest. Record the same
value in `deploy/.env` so Compose renders the intended release.
Migrations 0089–0091 change write contracts and are not compatible with an old
serving replica. Use the maintained stop-the-world command for this release and
for any later release that does not explicitly promise rolling compatibility:

```bash
cd deploy
export RSCTF_IMAGE=ghcr.io/dimasma0305/rsctf@sha256:<verified-release-digest>
docker compose pull
../scripts/compose-maintenance-cutover.sh \
  --project-name "${COMPOSE_PROJECT_NAME:-rsctf}" \
  --project-directory "$PWD" \
  --env-file .env \
  --image "$RSCTF_IMAGE"
docker compose ps
docker compose logs --tail=200 rsctf
```

The command captures replica counts, stops every `all`, `web`, `control`,
`engine`, and `network` container (including project-scoped strays), verifies
none can write, runs the new digest's `migrate` process, removes the stopped old
binaries, and starts only the new digest. Its mode-0600 state file makes a
failed attempt retryable. On migration failure it deliberately leaves the old
containers stopped; do not start the old image against the migrated database.
There is no automatic downgrade, so rollback after migration requires restoring
the matching pre-update database and file backup.

Migration 0090 bootstraps legacy identity observations as global-only evidence:
historical membership-link time was never recorded, so the upgrade does not
invent game-scoped attribution for old logins. Users already playing begin
game-scoped identity evidence with their next accepted login after cutover.

The first upgrade to installation-scoped Docker workload labels needs a short
maintenance drain. Stop competitive game orchestration, inspect legacy
containers selected by `docker ps -a --filter label=rsctf.managed=true`, and
remove only containers confirmed obsolete for this installation. Then set a
stable `RSCTF_DOCKER_SCOPE` and start every replica with that same value. The
scoped orphan sweeper and lifecycle API deliberately ignore legacy unscoped
containers: this prevents a new installation from adopting or deleting another
installation's workloads on a shared daemon. Manually remove every verified
legacy workload during this drain; afterward, recreate any still-needed
challenge instances so their containers carry the installation scope.

The explicit cutover covers both the default `all` topology and split roles.
Pin one digest for the entire sequence; runtime readiness also rejects required
peers built from different source. The [scaling guide](./scaling.md#migration-ownership)
describes the recovery contract; `web`, `control`, `engine`, and `network` never
run migrations themselves.

The bundled database is PostgreSQL 18. A PostgreSQL major-version change cannot
reuse an older server's data directory. For an existing PostgreSQL 16/17 managed
volume, take a logical dump and restore it into a fresh PostgreSQL 18 volume, or
follow PostgreSQL's `pg_upgrade` procedure. Merely changing the container tag is
not an upgrade and PostgreSQL will refuse the old data directory.

### Diagnose database load

The bundled Compose and Helm databases enable bounded `pg_stat_statements`
telemetry. Their init script creates the extension on a new volume; on an
existing PostgreSQL 18 volume, run
`CREATE EXTENSION IF NOT EXISTS pg_stat_statements;` once as the database owner.
PostgreSQL 18 also exposes asynchronous I/O activity through
`pg_aios` and byte totals in `pg_stat_io`. These views distinguish expensive SQL
and physical I/O from memory that PostgreSQL or the kernel is deliberately using
as a cache:

```sh
docker compose exec db sh -lc 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -P pager=off'
```

```sql
SELECT calls, round(total_exec_time::numeric, 1) AS total_ms,
       round(mean_exec_time::numeric, 2) AS mean_ms, rows,
       left(query, 120) AS query
FROM pg_stat_statements
ORDER BY total_exec_time DESC
LIMIT 20;

SELECT backend_type, object, context, reads, read_bytes, hits, evictions
FROM pg_stat_io
ORDER BY read_bytes DESC NULLS LAST;

SELECT state, operation, count(*)
FROM pg_aios
GROUP BY state, operation
ORDER BY state, operation;
```

Treat container RSS as a limit and trend signal, not as proof of a leak: it
includes useful database and filesystem caches. Investigate sustained growth
together with connection count, query latency, temporary bytes, and the views
above.

## Update Helm

The bundled chart now uses PostgreSQL 18. An existing chart PVC from PostgreSQL
16/17 contains a nested `pgdata` cluster that PostgreSQL 18 cannot reuse. The
chart's init guard deliberately blocks that rollout before a second empty
cluster can be initialized on the same PVC. Before `helm upgrade`, restore a
logical dump into a new PostgreSQL 18 PVC/database, complete a documented
`pg_upgrade`, or explicitly choose a new empty PVC when old data is meant to be
discarded. Changing only `postgresql.image.tag` is not an upgrade.

Pause GitOps reconcilers and scheduled operators, back up, and drain PgBouncer,
monitoring, and interactive sessions connected to the rsctf database. The
migration preflight conservatively refuses every other logged-in session in the
same database, regardless of database role. Resolve the reviewed manifest to an
immutable digest, then run the maintenance cutover. For the default `all`
release:

```bash
export RSCTF_VERSION=1.2.3
export RSCTF_IMAGE_REPOSITORY=ghcr.io/dimasma0305/rsctf
export RSCTF_IMAGE_DIGEST=sha256:<verified-manifest-digest>
scripts/kubernetes-maintenance-cutover.sh \
  --namespace rsctf-system \
  --chart oci://ghcr.io/dimasma0305/charts/rsctf \
  --chart-version "$RSCTF_VERSION" \
  --image-repository "$RSCTF_IMAGE_REPOSITORY" \
  --image-digest "$RSCTF_IMAGE_DIGEST" \
  --database-secret rsctf \
  --migrate-release rsctf-migrate \
  --runtime-release rsctf
kubectl -n rsctf-system rollout status deployment/rsctf
```

For split Helm, repeat `--runtime-release` for the exact `web` and `control`
releases, or for the exact `web`, `engine`, and `network` releases. The command
rejects unlisted related Deployments, HPAs, unsupported role combinations, and
singleton replica drift. It scales all old Deployments to zero and waits for
their Pods to terminate before installing the digest-scoped migration Job with
both `--wait` and `--wait-for-jobs`. A failed Job leaves every runtime at zero;
rerun the same command after fixing the cause. Helm's stored values preserve the
replica counts across that retry. The script never rolls back an old image after
the schema changes. See [one release per role](./scaling.md#helm-one-release-per-role).

Do not rotate the JWT secret during a normal update; doing so logs out every user. Do not change a bundled PostgreSQL password without also changing the password inside the existing database.

## Stop or uninstall Docker

```bash
cd deploy
docker compose stop                         # temporary stop
docker compose down --remove-orphans       # remove services, keep data
```

The destructive command below removes named data volumes:

```bash
docker compose down --volumes --remove-orphans
```

Do not run it unless a tested backup exists and permanent data deletion is intended. Dynamically created challenge containers are separate Docker objects; review containers carrying both `rsctf.managed` and `rsctf.scope` labels before deleting them. Their values are hashed installation identities, not secrets.
