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

Keep `deploy/.env` in an encrypted secret backup. Without its JWT secret and database password, recovery becomes more disruptive. Never commit it to Git.

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

Back up first. Then pull the selected published image:

```bash
cd deploy
docker compose pull
docker compose up -d --remove-orphans
docker compose ps
docker compose logs --tail=200 rsctf
```

rsctf applies forward database migrations at startup. There is no automatic downgrade, so a rollback may require restoring the pre-update database and file backup.

That automatic migration applies only to the default `RSCTF_ROLE=all` process.
For a split-role deployment, start PostgreSQL and Redis, run exactly one
one-shot `migrate` process, require it to succeed, and only then update the
long-running roles. Pin one non-latest image reference for that entire sequence;
runtime readiness rejects required peers built from different source. The
[scaling guide](./scaling.md#migration-ownership) shows
the Compose command; `web`, `control`, `engine`, and `network` never migrate.

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

Pin a reviewed image tag, back up, then change `image.tag` in the private values file:

```bash
export RSCTF_VERSION=1.2.3
helm upgrade rsctf oci://ghcr.io/dimasma0305/charts/rsctf \
  --version "$RSCTF_VERSION" \
  --namespace rsctf-system \
  --values rsctf-values.yaml \
  --wait
kubectl -n rsctf-system rollout status deployment/rsctf
```

The example above updates the default `all` release. In a split Helm topology,
upgrade the one-shot `runtimeRole=migrate` release first and wait for its hook
Job to succeed, then upgrade each `web`, `control`, `engine`, or `network`
release to the same image tag. See [one release per role](./scaling.md#helm-one-release-per-role).

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

Do not run it unless a tested backup exists and permanent data deletion is intended. Dynamically created challenge containers are separate Docker objects; review containers labeled `rsctf.managed=true` before deleting them.
