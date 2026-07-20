// Read-only evidence sampler for long-running load tests.
//
//   OUT_DIR=/tmp/rsctf-run \
//   TARGET=https://tcp.1pc.tf \
//   HEALTH_URL=http://127.0.0.1:8080/livez \
//   INTERVAL_SECONDS=5 node observe.mjs
//
// TARGET is sampled through the public path at /livez. HEALTH_URL is the local
// process-liveness endpoint; READINESS_URL separately checks PostgreSQL/cache
// readiness. The sampler never reads application secrets.
import { execFile } from 'node:child_process';
import { appendFile, rename, statfs, writeFile } from 'node:fs/promises';
import { arch, cpus, freemem, hostname, loadavg, platform, release, totalmem, uptime } from 'node:os';
import { join, resolve } from 'node:path';
import { promisify } from 'node:util';

import {
  OBSERVER_METADATA_SCHEMA_VERSION,
  assertObserverOutputOutsideRepository,
  createFreshObserverDirectory,
  gitWorktreeMetadata,
  observerComposeProjectName,
  readUntrackedWorktreeFiles,
} from './observer-evidence.js';

const execFileAsync = promisify(execFile);
const startedAt = new Date();
const runStamp = startedAt.toISOString().replaceAll(':', '').replaceAll('.', '-');
const repoRoot = resolve(new URL('../..', import.meta.url).pathname);
const outDir = resolve(process.env.OUT_DIR || `/tmp/rsctf-observe-${runStamp}`);
const intervalSeconds = Number(process.env.INTERVAL_SECONDS || 5);
if (!Number.isFinite(intervalSeconds) || intervalSeconds < 1) {
  throw new Error(`INTERVAL_SECONDS must be a finite number >= 1 (got ${process.env.INTERVAL_SECONDS})`);
}
const intervalMs = intervalSeconds * 1000;
const composeProjectName = observerComposeProjectName(process.env.COMPOSE_PROJECT_NAME);

const publicHealthUrl = healthUrlFromTarget(process.env.TARGET || 'http://127.0.0.1:8080');
const localHealthUrl = safeUrl(process.env.HEALTH_URL || 'http://127.0.0.1:8080/livez');
const readinessUrl = safeUrl(process.env.READINESS_URL || 'http://127.0.0.1:8080/healthz');
const containers = Object.freeze({
  rsctf: process.env.RSCTF_CONTAINER || 'rsctf-rsctf-1',
  postgres: process.env.PG_CONTAINER || 'rsctf-db-1',
  redis: process.env.REDIS_CONTAINER || 'rsctf-redis-1',
});
const postgres = Object.freeze({
  user: process.env.PG_USER || 'postgres',
  database: process.env.PG_DATABASE || 'rsctf',
});

const files = Object.freeze({
  publicHealth: join(outDir, 'health-public.csv'),
  localHealth: join(outDir, 'health-local.csv'),
  readiness: join(outDir, 'health-readiness.csv'),
  docker: join(outDir, 'docker-stats.csv'),
  redis: join(outDir, 'redis.csv'),
  postgres: join(outDir, 'postgres.csv'),
  host: join(outDir, 'host.csv'),
  fleet: join(outDir, 'fleet-stats.csv'),
  counts: join(outDir, 'container-counts.csv'),
  errors: join(outDir, 'errors.ndjson'),
  metadata: join(outDir, 'metadata.json'),
  summary: join(outDir, 'summary.json'),
});

const headers = Object.freeze({
  [files.publicHealth]: [
    'timestamp',
    'url',
    'ok',
    'status',
    'dnsMs',
    'connectMs',
    'tlsMs',
    'ttfbMs',
    'totalMs',
    'error',
  ],
  [files.localHealth]: [
    'timestamp',
    'url',
    'ok',
    'status',
    'dnsMs',
    'connectMs',
    'tlsMs',
    'ttfbMs',
    'totalMs',
    'error',
  ],
  [files.readiness]: [
    'timestamp',
    'url',
    'ok',
    'status',
    'dnsMs',
    'connectMs',
    'tlsMs',
    'ttfbMs',
    'totalMs',
    'error',
  ],
  [files.docker]: [
    'timestamp',
    'role',
    'container',
    'available',
    'cpuPercent',
    'memoryUsageBytes',
    'memoryLimitBytes',
    'memoryPercent',
    'netInputBytes',
    'netOutputBytes',
    'blockInputBytes',
    'blockOutputBytes',
    'pids',
    'memoryRaw',
    'netRaw',
    'blockRaw',
    'error',
  ],
  [files.redis]: [
    'timestamp',
    'available',
    'usedMemoryBytes',
    'usedMemoryRssBytes',
    'usedMemoryPeakBytes',
    'maxMemoryBytes',
    'maxMemoryPolicy',
    'memoryFragmentationRatio',
    'keys',
    'evictedKeys',
    'expiredKeys',
    'keyspaceHits',
    'keyspaceMisses',
    'rejectedConnections',
    'totalErrorReplies',
    'error',
  ],
  [files.postgres]: [
    'timestamp',
    'available',
    'connections',
    'activeConnections',
    'idleInTransactionConnections',
    'waitingConnections',
    'grantedLocks',
    'waitingLocks',
    'deadlocks',
    'commits',
    'rollbacks',
    'tempFiles',
    'tempBytes',
    'blocksRead',
    'blocksHit',
    'databaseSizeBytes',
    'longestTransactionSeconds',
    'error',
  ],
  [files.host]: [
    'timestamp',
    'load1',
    'load5',
    'load15',
    'load1PerCpu',
    'cpuCount',
    'memoryUsedBytes',
    'memoryFreeBytes',
    'memoryTotalBytes',
    'diskUsedBytes',
    'diskFreeBytes',
    'diskTotalBytes',
    'uptimeSeconds',
    'error',
  ],
  [files.fleet]: [
    'timestamp',
    'role',
    'available',
    'count',
    'cpuPercent',
    'memoryUsageBytes',
    'netInputBytes',
    'netOutputBytes',
    'blockInputBytes',
    'blockOutputBytes',
    'pids',
    'error',
  ],
  [files.counts]: [
    'timestamp',
    'available',
    'total',
    'running',
    'rsctfComposeTotal',
    'rsctfComposeRunning',
    'managedTotal',
    'managedRunning',
    'operationTotal',
    'operationRunning',
    'lifecycleAgentsRunning',
    'loadAgentsRunning',
    'isolatedServicesRunning',
    'attackClientsRunning',
    'namedKothRunning',
    'error',
  ],
});

const fleetRoles = Object.freeze([
  ['relayAgents', /^lcbyoc_\d+$/],
  ['isolatedServices', /^lcbyoc_svc_\d+$/],
  ['attackClients', /^(?:lcattack_|lck6_|lcteam_)/],
]);

// Three batches cover the expected 300-container event in one sampling wave.
// The cap avoids one huge Docker stats stream while keeping a stuck daemon call
// from delaying the remaining collectors indefinitely.
const dockerStatsBatchSize = 100;
const dockerStatsConcurrency = 3;
const dockerStatsTimeoutMs = 30_000;

// Deliberately allowlist only non-secret run controls. Never serialize the full
// environment: lifecycle JWTs, database credentials, and BYOC capabilities live there.
const reproducibilityEnvKeys = Object.freeze([
  'RUN_LABEL',
  'VUS',
  'RATE',
  'FLEET',
  'DURATION',
  'N',
  'TEAMS_JEO',
  'TEAMS_AD',
  'CH_STATIC',
  'GAME',
  'CID',
  'CONTAINER_EVERY',
  'AD_ATTACK_CONCURRENCY',
  'AD_MIN_ATTACK_ROUNDS',
  'LIFECYCLE_ISOLATED_SERVICES',
  'BYOC_ATTACK_PATH',
  'PLAYER_THINK_SECONDS',
  'STABLE_IPS',
  'EVENT_DURATION_SECONDS',
  'EVENT_HIDDEN',
  'ALIGN_EVENT_END',
  'EVENT_END_GRACE_SECONDS',
  'EVENT_SETTLEMENT_TIMEOUT_SECONDS',
  'EPOCH_READY_TIMEOUT_SECONDS',
  'CROWN_READY_TIMEOUT_SECONDS',
  'CROWN_MIN_ACQUISITIONS',
  'CROWN_MIN_COMPLETED',
  'CROWN_MIN_STALE_REJECTIONS',
  'REQUIRE_ISOLATED_SERVICES',
  'DISTRIBUTED_TEAM_CLIENTS',
  'REALISTIC_COMPETITION',
  'SIMULATION_SEED',
  'COMPETITION_RUN_ID',
  'LIFECYCLE_STATE_TAG',
  'INTEGRATED_CHEAT_SIMULATION',
  'CHEAT_AT_FRACTION',
  'RETAIN_EVENT',
  'TEAM_CLIENT_CPUS',
  'TEAM_CLIENT_MEMORY',
  'TEAM_THINK_SECONDS',
  'TEAM_START_DELAY_SECONDS',
  'NOKEEPALIVE',
  'CONTAINER_IMAGE',
  'RSCTF_BYOC_AGENT_IMAGE',
  'RSCTF_BYOC_SERVICE_IMAGE',
  'RSCTF_BYOC_RUN_ID',
  'RSCTF_ACCEPTANCE_REPORTABLE',
  'COMPOSE_PROJECT_NAME',
]);

const summary = {
  startedAt: startedAt.toISOString(),
  endedAt: null,
  stopSignal: null,
  samples: 0,
  collectorErrors: {},
  health: {
    public: { ok: 0, failed: 0 },
    local: { ok: 0, failed: 0 },
    readiness: { ok: 0, failed: 0 },
  },
};

let stopping = false;
let sleepTimer = null;
let wakeSleep = null;
let traefikContainer = null;
let ownsOutputDirectory = false;

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.once(signal, () => {
    if (stopping) return;
    stopping = true;
    summary.stopSignal = signal;
    if (sleepTimer) clearTimeout(sleepTimer);
    wakeSleep?.();
  });
}

function safeUrl(value) {
  const parsed = new URL(value);
  parsed.username = '';
  parsed.password = '';
  parsed.search = '';
  parsed.hash = '';
  return parsed.toString();
}

function healthUrlFromTarget(value) {
  const parsed = new URL(value);
  parsed.username = '';
  parsed.password = '';
  parsed.pathname = '/livez';
  parsed.search = '';
  parsed.hash = '';
  return parsed.toString();
}

function csv(value) {
  if (value === null || value === undefined) return '';
  const text = String(value);
  return /[",\r\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
}

async function appendCsv(file, values) {
  await appendFile(file, `${values.map(csv).join(',')}\n`);
}

function cleanError(error) {
  const message = error instanceof Error ? error.message : String(error);
  return message.replaceAll(/\s+/g, ' ').trim().slice(0, 500);
}

function sanitizedMetadataValue(value) {
  return String(value)
    .replaceAll(/([a-z][a-z0-9+.-]*:\/\/)[^/@\s]+@/gi, '$1<redacted>@')
    .replaceAll(/((?:password|secret|token|authorization|cookie|api[_-]?key)\s*[=:]\s*)[^,;\s&]+/gi, '$1<redacted>')
    .replaceAll(/[\u0000-\u001f\u007f]/g, ' ')
    .trim()
    .slice(0, 256);
}

function reproducibilityEnvironment() {
  return Object.fromEntries(
    reproducibilityEnvKeys.flatMap((key) => {
      const value = process.env[key];
      return value === undefined ? [] : [[key, sanitizedMetadataValue(value)]];
    })
  );
}

async function recordError(timestamp, component, error) {
  const message = cleanError(error);
  summary.collectorErrors[component] = (summary.collectorErrors[component] || 0) + 1;
  await appendFile(files.errors, `${JSON.stringify({ timestamp, component, message })}\n`);
  return message;
}

async function command(file, args, timeout = 10_000, { rawStdout = false } = {}) {
  try {
    const { stdout, stderr } = await execFileAsync(file, args, {
      detached: true,
      encoding: rawStdout ? 'buffer' : 'utf8',
      maxBuffer: 4 * 1024 * 1024,
      timeout,
    });
    return {
      ok: true,
      stdout: rawStdout ? Buffer.from(stdout) : stdout.trim(),
      stderr: String(stderr).trim(),
      error: '',
    };
  } catch (error) {
    return {
      ok: false,
      stdout: rawStdout
        ? Buffer.from(error.stdout || [])
        : String(error.stdout || '').trim(),
      stderr: String(error.stderr || '').trim(),
      error: cleanError(error.stderr || error),
    };
  }
}

function number(value, fallback = 0) {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function parseBytes(value) {
  const match = String(value || '')
    .trim()
    .match(/^([\d.]+)\s*([kmgtpe]?i?b)?$/i);
  if (!match) return 0;
  const units = {
    b: 1,
    kb: 1_000,
    mb: 1_000_000,
    gb: 1_000_000_000,
    tb: 1_000_000_000_000,
    pb: 1_000_000_000_000_000,
    eb: 1_000_000_000_000_000_000,
    kib: 1024,
    mib: 1024 ** 2,
    gib: 1024 ** 3,
    tib: 1024 ** 4,
    pib: 1024 ** 5,
    eib: 1024 ** 6,
  };
  return Math.round(Number(match[1]) * (units[(match[2] || 'b').toLowerCase()] || 1));
}

function parsePair(value) {
  const [left, right] = String(value || '')
    .split('/')
    .map((part) => part.trim());
  return [parseBytes(left), parseBytes(right)];
}

function parseInfo(value) {
  const result = {};
  for (const line of String(value || '').split(/\r?\n/)) {
    if (!line || line.startsWith('#')) continue;
    const separator = line.indexOf(':');
    if (separator <= 0) continue;
    result[line.slice(0, separator)] = line.slice(separator + 1).trim();
  }
  return result;
}

async function collectHealth(timestamp, kind, url, file) {
  const format = [
    '%{http_code}',
    '%{time_namelookup}',
    '%{time_connect}',
    '%{time_appconnect}',
    '%{time_starttransfer}',
    '%{time_total}',
  ].join('\t');
  const result = await command(
    'curl',
    [
      '--silent',
      '--show-error',
      '--location',
      '--max-redirs',
      '3',
      '--max-time',
      '4',
      '--output',
      '/dev/null',
      '--write-out',
      format,
      url,
    ],
    6_000
  );
  const fields = result.stdout.split('\t');
  const status = number(fields[0]);
  const ok = result.ok && status >= 200 && status < 300;
  const error = result.ok ? '' : await recordError(timestamp, `health-${kind}`, result.error);
  summary.health[kind][ok ? 'ok' : 'failed']++;
  await appendCsv(file, [
    timestamp,
    url,
    ok,
    status,
    number(fields[1]) * 1000,
    number(fields[2]) * 1000,
    number(fields[3]) * 1000,
    number(fields[4]) * 1000,
    number(fields[5]) * 1000,
    error,
  ]);
}

async function discoverTraefik() {
  if (process.env.TRAEFIK_CONTAINER) return process.env.TRAEFIK_CONTAINER;
  const result = await command('docker', ['ps', '--format', '{{.Names}}\t{{.Label "com.docker.compose.service"}}']);
  if (!result.ok) return null;
  const rows = result.stdout
    .split('\n')
    .filter(Boolean)
    .map((line) => line.split('\t'));
  return (
    rows.find(([, service]) => service === 'traefik')?.[0] ||
    rows.find(([name]) => /(^|[-_])traefik([-_]|$)/i.test(name))?.[0] ||
    null
  );
}

async function collectDockerStats(timestamp) {
  const wanted = [
    ['rsctf', containers.rsctf],
    ['postgres', containers.postgres],
    ['redis', containers.redis],
  ];
  if (traefikContainer && !wanted.some(([, name]) => name === traefikContainer)) {
    wanted.push(['traefik', traefikContainer]);
  }
  const result = await command(
    'docker',
    ['stats', '--no-stream', '--format', '{{json .}}', ...wanted.map(([, name]) => name)],
    12_000
  );
  const models = new Map();
  for (const line of result.stdout.split('\n').filter(Boolean)) {
    try {
      const model = JSON.parse(line);
      models.set(model.Name || model.Container, model);
    } catch (error) {
      await recordError(timestamp, 'docker-stats', error);
    }
  }
  if (stopping && models.size !== wanted.length) return;
  if (!result.ok && !stopping) await recordError(timestamp, 'docker-stats', result.error);

  await Promise.all(
    wanted.map(async ([role, name]) => {
      const model = models.get(name);
      if (!model) {
        const error = result.error || 'container unavailable';
        await appendCsv(files.docker, [
          timestamp,
          role,
          name,
          false,
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          error,
        ]);
        return;
      }
      try {
        const [memoryUsage, memoryLimit] = parsePair(model.MemUsage);
        const [netInput, netOutput] = parsePair(model.NetIO);
        const [blockInput, blockOutput] = parsePair(model.BlockIO);
        await appendCsv(files.docker, [
          timestamp,
          role,
          model.Name || name,
          true,
          number(String(model.CPUPerc || '').replace('%', '')),
          memoryUsage,
          memoryLimit,
          number(String(model.MemPerc || '').replace('%', '')),
          netInput,
          netOutput,
          blockInput,
          blockOutput,
          number(model.PIDs),
          model.MemUsage || '',
          model.NetIO || '',
          model.BlockIO || '',
          '',
        ]);
      } catch (error) {
        const message = await recordError(timestamp, `docker-${role}`, error);
        await appendCsv(files.docker, [
          timestamp,
          role,
          name,
          false,
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          '',
          message,
        ]);
      }
    })
  );
}

function fleetRoleFor(name) {
  return fleetRoles.find(([, pattern]) => pattern.test(name))?.[0] || null;
}

async function batchedFleetStats(timestamp, names) {
  const batches = [];
  for (let offset = 0; offset < names.length; offset += dockerStatsBatchSize) {
    batches.push(names.slice(offset, offset + dockerStatsBatchSize));
  }

  const models = [];
  const failures = [];
  let nextBatch = 0;
  async function worker() {
    while (nextBatch < batches.length) {
      const batch = batches[nextBatch++];
      const result = await command(
        'docker',
        ['stats', '--no-stream', '--format', '{{json .}}', ...batch],
        dockerStatsTimeoutMs
      );
      for (const line of result.stdout.split('\n').filter(Boolean)) {
        try {
          models.push(JSON.parse(line));
        } catch (error) {
          failures.push(cleanError(error));
        }
      }
      if (!result.ok) failures.push(result.error || 'Docker stats batch failed');
    }
  }

  const workerCount = Math.min(dockerStatsConcurrency, batches.length);
  await Promise.all(Array.from({ length: workerCount }, worker));

  const returnedNames = new Set(
    models.map((model) => model.Name).filter((name) => typeof name === 'string' && name !== '')
  );
  const missingNames = names.filter((name) => !returnedNames.has(name));
  if (missingNames.length > 0) {
    failures.push(
      `Docker stats omitted ${missingNames.length}/${names.length} requested container(s): ` +
        `${missingNames.slice(0, 5).join(',')}${missingNames.length > 5 ? ',…' : ''}`
    );
  }
  for (const failure of failures) {
    if (!stopping) await recordError(timestamp, 'fleet-stats', failure);
  }
  return { models, failures };
}

async function collectFleetStats(timestamp) {
  const inventory = await command('docker', ['ps', '--format', '{{.Names}}'], 15_000);
  if (!inventory.ok) {
    const detail = inventory.error || 'Docker returned no fleet inventory';
    const error = stopping ? detail : await recordError(timestamp, 'fleet-stats', detail);
    await Promise.all(
      fleetRoles.map(([role]) =>
        appendCsv(files.fleet, [timestamp, role, false, '', '', '', '', '', '', '', '', error])
      )
    );
    return;
  }

  const names = [...new Set(inventory.stdout.split('\n').filter((name) => fleetRoleFor(name)))];
  const { models, failures } = await batchedFleetStats(timestamp, names);
  const aggregates = new Map(
    fleetRoles.map(([role]) => [
      role,
      {
        count: names.filter((name) => fleetRoleFor(name) === role).length,
        cpuPercent: 0,
        memoryUsageBytes: 0,
        netInputBytes: 0,
        netOutputBytes: 0,
        blockInputBytes: 0,
        blockOutputBytes: 0,
        pids: 0,
      },
    ])
  );

  for (const model of models) {
    const role = fleetRoleFor(model.Name || model.Container || '');
    if (!role) continue;
    const aggregate = aggregates.get(role);
    const [memoryUsage] = parsePair(model.MemUsage);
    const [netInput, netOutput] = parsePair(model.NetIO);
    const [blockInput, blockOutput] = parsePair(model.BlockIO);
    aggregate.cpuPercent += number(String(model.CPUPerc || '').replace('%', ''));
    aggregate.memoryUsageBytes += memoryUsage;
    aggregate.netInputBytes += netInput;
    aggregate.netOutputBytes += netOutput;
    aggregate.blockInputBytes += blockInput;
    aggregate.blockOutputBytes += blockOutput;
    aggregate.pids += number(model.PIDs);
  }

  const available = failures.length === 0;
  const error = available ? '' : `${failures.length} Docker stats batch error(s)`;
  await Promise.all(
    fleetRoles.map(([role]) => {
      const aggregate = aggregates.get(role);
      return appendCsv(files.fleet, [
        timestamp,
        role,
        available,
        aggregate.count,
        aggregate.cpuPercent,
        aggregate.memoryUsageBytes,
        aggregate.netInputBytes,
        aggregate.netOutputBytes,
        aggregate.blockInputBytes,
        aggregate.blockOutputBytes,
        aggregate.pids,
        error,
      ]);
    })
  );
}

async function collectRedis(timestamp) {
  const result = await command('docker', ['exec', containers.redis, 'redis-cli', '--raw', 'INFO', 'all']);
  if (!result.ok || !result.stdout) {
    const detail = result.error || 'Redis returned an empty INFO response';
    const error = stopping ? detail : await recordError(timestamp, 'redis', detail);
    await appendCsv(files.redis, [timestamp, false, '', '', '', '', '', '', '', '', '', '', '', '', '', error]);
    return;
  }
  const info = parseInfo(result.stdout);
  const keys = Number(String(info.db0 || '').match(/(?:^|,)keys=(\d+)/)?.[1] || 0);
  await appendCsv(files.redis, [
    timestamp,
    true,
    number(info.used_memory),
    number(info.used_memory_rss),
    number(info.used_memory_peak),
    number(info.maxmemory),
    info.maxmemory_policy || '',
    number(info.mem_fragmentation_ratio),
    keys,
    number(info.evicted_keys),
    number(info.expired_keys),
    number(info.keyspace_hits),
    number(info.keyspace_misses),
    number(info.rejected_connections),
    number(info.total_error_replies),
    '',
  ]);
}

const postgresQuery = String.raw`
WITH activity AS (
  SELECT count(*)::bigint AS connections,
         count(*) FILTER (WHERE state = 'active')::bigint AS active_connections,
         count(*) FILTER (WHERE state = 'idle in transaction')::bigint
           AS idle_in_transaction_connections,
         count(*) FILTER (WHERE state <> 'idle' AND wait_event IS NOT NULL)::bigint
           AS waiting_connections,
         COALESCE(max(extract(epoch FROM clock_timestamp() - xact_start))
           FILTER (WHERE xact_start IS NOT NULL), 0.0) AS longest_transaction_seconds
    FROM pg_stat_activity
   WHERE datname = current_database()
), locks AS (
  SELECT count(*) FILTER (WHERE granted)::bigint AS granted_locks,
         count(*) FILTER (WHERE NOT granted)::bigint AS waiting_locks
    FROM pg_locks lock_state
    LEFT JOIN pg_database database_state ON database_state.oid = lock_state.database
   WHERE lock_state.database IS NULL OR database_state.datname = current_database()
)
SELECT json_build_object(
  'connections', activity.connections,
  'activeConnections', activity.active_connections,
  'idleInTransactionConnections', activity.idle_in_transaction_connections,
  'waitingConnections', activity.waiting_connections,
  'grantedLocks', locks.granted_locks,
  'waitingLocks', locks.waiting_locks,
  'deadlocks', database_stats.deadlocks,
  'commits', database_stats.xact_commit,
  'rollbacks', database_stats.xact_rollback,
  'tempFiles', database_stats.temp_files,
  'tempBytes', database_stats.temp_bytes,
  'blocksRead', database_stats.blks_read,
  'blocksHit', database_stats.blks_hit,
  'databaseSizeBytes', pg_database_size(current_database()),
  'longestTransactionSeconds', activity.longest_transaction_seconds
)
FROM pg_stat_database database_stats CROSS JOIN activity CROSS JOIN locks
WHERE database_stats.datname = current_database()`;

async function collectPostgres(timestamp) {
  const result = await command('docker', [
    'exec',
    containers.postgres,
    'psql',
    '-U',
    postgres.user,
    '-d',
    postgres.database,
    '-X',
    '-qAt',
    '-c',
    postgresQuery,
  ]);
  if (!result.ok) {
    const error = await recordError(timestamp, 'postgres', result.error);
    await appendCsv(files.postgres, [
      timestamp,
      false,
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      error,
    ]);
    return;
  }
  try {
    const model = JSON.parse(result.stdout);
    await appendCsv(files.postgres, [
      timestamp,
      true,
      model.connections,
      model.activeConnections,
      model.idleInTransactionConnections,
      model.waitingConnections,
      model.grantedLocks,
      model.waitingLocks,
      model.deadlocks,
      model.commits,
      model.rollbacks,
      model.tempFiles,
      model.tempBytes,
      model.blocksRead,
      model.blocksHit,
      model.databaseSizeBytes,
      model.longestTransactionSeconds,
      '',
    ]);
  } catch (error) {
    const message = await recordError(timestamp, 'postgres', error);
    await appendCsv(files.postgres, [
      timestamp,
      false,
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      '',
      message,
    ]);
  }
}

async function collectHost(timestamp) {
  try {
    const memoryTotal = totalmem();
    const memoryFree = freemem();
    const disk = await statfs('/');
    const blockSize = Number(disk.bsize);
    const diskTotal = blockSize * Number(disk.blocks);
    const diskFree = blockSize * Number(disk.bavail);
    const loads = loadavg();
    const cpuCount = cpus().length;
    await appendCsv(files.host, [
      timestamp,
      loads[0],
      loads[1],
      loads[2],
      cpuCount ? loads[0] / cpuCount : 0,
      cpuCount,
      memoryTotal - memoryFree,
      memoryFree,
      memoryTotal,
      diskTotal - diskFree,
      diskFree,
      diskTotal,
      uptime(),
      '',
    ]);
  } catch (error) {
    const message = await recordError(timestamp, 'host', error);
    await appendCsv(files.host, [timestamp, '', '', '', '', '', '', '', '', '', '', '', '', message]);
  }
}

async function collectContainerCounts(timestamp) {
  const result = await command('docker', ['ps', '-a', '--format', '{{json .}}']);
  if (!result.ok) {
    const error = await recordError(timestamp, 'container-counts', result.error);
    await appendCsv(files.counts, [timestamp, false, ...Array(13).fill(''), error]);
    return;
  }
  try {
    const rows = result.stdout
      .split('\n')
      .filter(Boolean)
      .map((line) => JSON.parse(line));
    const running = (row) => row.State === 'running';
    const hasLabel = (row, label) =>
      String(row.Labels || '')
        .split(',')
        .includes(label);
    const compose = (row) =>
      hasLabel(row, `com.docker.compose.project=${composeProjectName}`);
    const managed = (row) =>
      String(row.Labels || '')
        .split(',')
        .some((label) => label.startsWith('rsctf.managed='));
    const operation = (row) => String(row.Labels || '').includes('rsctf.operation=');
    const lifecycleAgent = (row) => /^lcbyoc_\d+$/.test(row.Names || '');
    const isolatedService = (row) => /^lcbyoc_svc_\d+$/.test(row.Names || '');
    const attackClient = (row) => /^(?:lcattack_|lck6_|lcteam_)/.test(row.Names || '');
    const loadAgent = (row) =>
      /^load_agent_(?:[a-z0-9][a-z0-9-]{0,47}_)?\d+$/.test(row.Names || '') ||
      (hasLabel(row, 'rsctf.load.byoc.owner=byoc-stress-v1') &&
        hasLabel(row, 'rsctf.load.byoc.role=relay'));
    const namedKoth = (row) => /^lckoth_/.test(row.Names || '');
    await appendCsv(files.counts, [
      timestamp,
      true,
      rows.length,
      rows.filter(running).length,
      rows.filter(compose).length,
      rows.filter((row) => compose(row) && running(row)).length,
      rows.filter(managed).length,
      rows.filter((row) => managed(row) && running(row)).length,
      rows.filter(operation).length,
      rows.filter((row) => operation(row) && running(row)).length,
      rows.filter((row) => lifecycleAgent(row) && running(row)).length,
      rows.filter((row) => loadAgent(row) && running(row)).length,
      rows.filter((row) => isolatedService(row) && running(row)).length,
      rows.filter((row) => attackClient(row) && running(row)).length,
      rows.filter((row) => namedKoth(row) && running(row)).length,
      '',
    ]);
  } catch (error) {
    const message = await recordError(timestamp, 'container-counts', error);
    await appendCsv(files.counts, [timestamp, false, ...Array(13).fill(''), message]);
  }
}

async function createCsv(file, header) {
  await writeFile(file, `${header.map(csv).join(',')}\n`, {
    flag: 'wx',
    mode: 0o640,
  });
}

async function atomicJson(file, value) {
  const temporary = `${file}.${process.pid}.tmp`;
  await writeFile(temporary, `${JSON.stringify(value, null, 2)}\n`, {
    mode: 0o640,
  });
  await rename(temporary, file);
}

async function metadata() {
  const [dockerVersion, gitRevision, gitStatus, gitDiff, gitUntracked] = await Promise.all([
    command('docker', ['version', '--format', '{{.Server.Version}}']),
    command('git', ['-C', repoRoot, 'rev-parse', '--verify', 'HEAD']),
    command('git', ['-C', repoRoot, 'status', '--porcelain=v1', '--untracked-files=all']),
    command(
      'git',
      ['-C', repoRoot, 'diff', '--no-ext-diff', '--binary', 'HEAD', '--', '.'],
      10_000,
      { rawStdout: true },
    ),
    command('git', ['-C', repoRoot, 'ls-files', '--others', '--exclude-standard', '-z']),
  ]);
  if (!gitRevision.ok || !gitStatus.ok || !gitDiff.ok || !gitUntracked.ok) {
    throw new Error(
      `cannot bind observer metadata to the worktree: ` +
        [gitRevision, gitStatus, gitDiff, gitUntracked]
          .filter((result) => !result.ok)
          .map((result) => result.error)
          .join('; ')
    );
  }
  const untrackedFiles = await readUntrackedWorktreeFiles(
    repoRoot,
    gitUntracked.stdout.split('\0').filter(Boolean),
  );
  const git = gitWorktreeMetadata({
    revision: gitRevision.stdout,
    status: gitStatus.stdout,
    trackedDiff: gitDiff.stdout,
    untrackedFiles,
  });
  traefikContainer = await discoverTraefik();
  return {
    schemaVersion: OBSERVER_METADATA_SCHEMA_VERSION,
    startedAt: startedAt.toISOString(),
    endedAt: null,
    outputDirectory: outDir,
    competitionRunId: process.env.COMPETITION_RUN_ID
      ? sanitizedMetadataValue(process.env.COMPETITION_RUN_ID)
      : null,
    intervalSeconds,
    endpoints: {
      publicHealth: publicHealthUrl,
      localHealth: localHealthUrl,
      readiness: readinessUrl,
    },
    containers: { ...containers, traefik: traefikContainer },
    runtime: {
      node: process.version,
      dockerServer: dockerVersion.ok ? dockerVersion.stdout : null,
      ...git,
    },
    reproducibilityEnvironment: reproducibilityEnvironment(),
    host: {
      hostname: hostname(),
      platform: platform(),
      release: release(),
      architecture: arch(),
      cpuCount: cpus().length,
      totalMemoryBytes: totalmem(),
    },
  };
}

async function sample() {
  const timestamp = new Date().toISOString();
  await Promise.all([
    collectHealth(timestamp, 'public', publicHealthUrl, files.publicHealth),
    collectHealth(timestamp, 'local', localHealthUrl, files.localHealth),
    collectHealth(timestamp, 'readiness', readinessUrl, files.readiness),
    collectDockerStats(timestamp),
    collectFleetStats(timestamp),
    collectRedis(timestamp),
    collectPostgres(timestamp),
    collectHost(timestamp),
    collectContainerCounts(timestamp),
  ]);
  summary.samples++;
}

async function interruptibleSleep(milliseconds) {
  if (stopping || milliseconds <= 0) return;
  await new Promise((resolveSleep) => {
    let resolved = false;
    const finish = () => {
      if (resolved) return;
      resolved = true;
      sleepTimer = null;
      wakeSleep = null;
      resolveSleep();
    };
    wakeSleep = finish;
    sleepTimer = setTimeout(finish, milliseconds);
  });
}

async function main() {
  await assertObserverOutputOutsideRepository(repoRoot, outDir);
  await createFreshObserverDirectory(outDir);
  ownsOutputDirectory = true;
  await Promise.all(Object.entries(headers).map(([file, header]) => createCsv(file, header)));
  const runMetadata = await metadata();
  await atomicJson(files.metadata, runMetadata);
  process.stdout.write(
    `observing every ${intervalSeconds}s → ${outDir}\n` +
      `  public ${publicHealthUrl}\n` +
      `  local  ${localHealthUrl}\n` +
      `  ready  ${readinessUrl}\n` +
      'press Ctrl-C to stop after the current read-only sample\n'
  );

  while (!stopping) {
    const sampleStarted = Date.now();
    await sample();
    await interruptibleSleep(Math.max(0, intervalMs - (Date.now() - sampleStarted)));
  }

  const endedAt = new Date().toISOString();
  summary.endedAt = endedAt;
  runMetadata.endedAt = endedAt;
  runMetadata.stopSignal = summary.stopSignal;
  runMetadata.samples = summary.samples;
  await Promise.all([atomicJson(files.metadata, runMetadata), atomicJson(files.summary, summary)]);
  process.stdout.write(`observer stopped cleanly after ${summary.samples} sample(s)\n`);
}

main().catch(async (error) => {
  const timestamp = new Date().toISOString();
  if (ownsOutputDirectory) {
    try {
      await recordError(timestamp, 'observer', error);
      summary.endedAt = timestamp;
      await atomicJson(files.summary, summary);
    } catch {
      // Preserve the original failure if this run's evidence directory is unavailable.
    }
  }
  console.error(`observer failed: ${cleanError(error)}`);
  process.exitCode = 1;
});
