# Back up and update

An rsctf backup is complete only when it contains PostgreSQL and the uploaded-file storage from the same operational period.

## Docker backup

Create a PostgreSQL dump from the managed Compose stack:

```bash
mkdir -p backups
cd deploy
docker compose exec -T db \
  pg_dump -U rsctf -d rsctf -Fc \
  > "../backups/rsctf-$(date -u +%Y%m%dT%H%M%SZ).dump"
```

Back up the named file volume as well. A portable method is to stop writes briefly and archive the mounted volume with a temporary container; determine the actual volume name with:

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
