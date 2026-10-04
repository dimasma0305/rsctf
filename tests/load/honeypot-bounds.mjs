// Disposable/local fixed-rate gate for bounded HTTP/TCP honeypot telemetry.
import { spawn, spawnSync } from 'node:child_process';
import net from 'node:net';
import { resolve } from 'node:path';
import { PG, RSCTF, sql, TARGET } from './lib.mjs';

const integer = (value, name, minimum, maximum) => {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${name} must be an integer in ${minimum}..${maximum}`);
  }
  return parsed;
};

if (process.env.HONEYPOT_STRESS_ACK !== '1') {
  throw new Error('HONEYPOT_STRESS_ACK=1 is required because this gate persists sampled decoy telemetry');
}
const target = new URL(TARGET);
if (
  !['127.0.0.1', 'localhost', '::1'].includes(target.hostname) &&
  process.env.ALLOW_REMOTE_HONEYPOT_STRESS !== target.origin
) {
  throw new Error(`remote TARGET requires ALLOW_REMOTE_HONEYPOT_STRESS=${target.origin}`);
}
const rate = integer(process.env.RATE || 512, 'RATE', 1, 10_000);
const vus = integer(process.env.VUS || 64, 'VUS', 1, 2_048);
const sourceCount = integer(process.env.SOURCE_COUNT || 16, 'SOURCE_COUNT', 1, 254);
const duration = String(process.env.DURATION || '20s');
const durationMatch = duration.match(/^([1-9]\d*)(s|m)$/);
const durationSeconds = durationMatch ? Number(durationMatch[1]) * (durationMatch[2] === 'm' ? 60 : 1) : 0;
if (durationSeconds < 1 || durationSeconds > 600) throw new Error('DURATION must be between 1s and 10m');
const limits = {
  cpuPercent: integer(process.env.MAX_CPU_PERCENT || 400, 'MAX_CPU_PERCENT', 1, 10_000),
  pgConnections: integer(process.env.MAX_PG_CONNECTIONS || 40, 'MAX_PG_CONNECTIONS', 1, 10_000),
  pgActiveConnections: integer(
    process.env.MAX_PG_ACTIVE_CONNECTIONS || 40,
    'MAX_PG_ACTIVE_CONNECTIONS',
    1,
    10_000,
  ),
  pgIdleInTransaction: integer(
    process.env.MAX_PG_IDLE_IN_TRANSACTION || 0,
    'MAX_PG_IDLE_IN_TRANSACTION',
    0,
    10_000,
  ),
  pgWaitingConnections: integer(
    process.env.MAX_PG_WAITING_CONNECTIONS || 16,
    'MAX_PG_WAITING_CONNECTIONS',
    0,
    10_000,
  ),
  pgLongestTransactionSeconds: integer(
    process.env.MAX_PG_LONGEST_TRANSACTION_SECONDS || 30,
    'MAX_PG_LONGEST_TRANSACTION_SECONDS',
    1,
    600,
  ),
  pgBlockReads: integer(
    process.env.MAX_PG_BLOCK_READ_DELTA || 100_000,
    'MAX_PG_BLOCK_READ_DELTA',
    1,
    10_000_000,
  ),
  pgTempMiB: integer(process.env.MAX_PG_TEMP_DELTA_MIB || 64, 'MAX_PG_TEMP_DELTA_MIB', 1, 65_536),
};
const tcpPort = process.env.HONEYPOT_TCP_PORT
  ? integer(process.env.HONEYPOT_TCP_PORT, 'HONEYPOT_TCP_PORT', 1, 65_535)
  : null;
const tcpConnections = integer(process.env.TCP_CONNECTIONS || 256, 'TCP_CONNECTIONS', 1, 2_048);
const maxRows = 10 * sourceCount * (Math.ceil(durationSeconds / 60) + 2);
const loadBaits = [
  '/.env',
  '/.git/config',
  '/.git/HEAD',
  '/wp-login.php',
  '/phpmyadmin',
  '/server-status',
  '/actuator/env',
  '/_ignition/execute-solution',
  '/backup.zip',
  '/database.sql',
];
const containers = String(process.env.HONEYPOT_RESOURCE_CONTAINERS || `${RSCTF},${PG}`)
  .split(',')
  .map((value) => value.trim())
  .filter(Boolean);
if (
  containers.length < 2 ||
  containers.length > 16 ||
  !containers.every((name) => /^[\w.-]{1,128}$/.test(name)) ||
  !containers.includes(RSCTF) ||
  !containers.includes(PG)
) {
  throw new Error(
    'HONEYPOT_RESOURCE_CONTAINERS requires 2..16 bounded names including RSCTF_CONTAINER and PG_CONTAINER',
  );
}

const parseBytes = (value) => {
  const match = String(value).trim().match(/^([0-9.]+)([kmgt]?i?b)$/i);
  if (!match) return null;
  const powers = { b: 0, kb: 1, kib: 1, mb: 2, mib: 2, gb: 3, gib: 3, tb: 4, tib: 4 };
  return Number(match[1]) * 1024 ** (powers[match[2].toLowerCase()] ?? 0);
};
const parsePercent = (value) => {
  const match = String(value).trim().match(/^([0-9]+(?:\.[0-9]+)?)%$/);
  return match ? Number(match[1]) : null;
};

const samples = [];
const databaseSamples = [];
const samplingErrors = [];
const databaseSample = () => {
  const raw = sql(
    `WITH activity AS (` +
      `SELECT COUNT(*)::BIGINT AS pool_connections, ` +
      `COUNT(*) FILTER (WHERE state='active')::BIGINT AS active_connections, ` +
      `COUNT(*) FILTER (WHERE state LIKE 'idle in transaction%')::BIGINT AS idle_in_transaction_connections, ` +
      `COUNT(*) FILTER (WHERE state<>'idle' AND wait_event IS NOT NULL)::BIGINT AS waiting_connections, ` +
      `COALESCE(MAX(EXTRACT(EPOCH FROM clock_timestamp()-xact_start)) ` +
      `FILTER (WHERE xact_start IS NOT NULL), 0.0) AS longest_transaction_seconds ` +
      `FROM pg_stat_activity WHERE datname=current_database() AND backend_type='client backend' ` +
      `AND pid<>pg_backend_pid()) ` +
      `SELECT json_build_object(` +
      `'poolConnections', activity.pool_connections, ` +
      `'activeConnections', activity.active_connections, ` +
      `'idleInTransactionConnections', activity.idle_in_transaction_connections, ` +
      `'waitingConnections', activity.waiting_connections, ` +
      `'longestTransactionSeconds', activity.longest_transaction_seconds, ` +
      `'blockReads', database_stats.blks_read, 'tempBytes', database_stats.temp_bytes)::text ` +
      `FROM activity CROSS JOIN pg_stat_database database_stats ` +
      `WHERE database_stats.datname=current_database()`,
  );
  const value = JSON.parse(raw);
  for (const key of [
    'poolConnections',
    'activeConnections',
    'idleInTransactionConnections',
    'waitingConnections',
    'blockReads',
    'tempBytes',
  ]) {
    value[key] = Number(value[key]);
    if (!Number.isSafeInteger(value[key]) || value[key] < 0) {
      throw new Error(`invalid PostgreSQL ${key} sample`);
    }
  }
  value.longestTransactionSeconds = Number(value.longestTransactionSeconds);
  if (!Number.isFinite(value.longestTransactionSeconds) || value.longestTransactionSeconds < 0) {
    throw new Error('invalid PostgreSQL longestTransactionSeconds sample');
  }
  return value;
};
const sample = () => {
  const stats = spawnSync('docker', ['stats', '--no-stream', '--format', '{{json .}}', ...containers], {
    encoding: 'utf8',
  });
  if (stats.status !== 0) throw new Error(`docker stats failed: ${(stats.stderr || stats.stdout || '').trim()}`);
  for (const line of stats.stdout.split('\n').filter(Boolean)) {
    const row = JSON.parse(line);
    const cpuPercent = parsePercent(row.CPUPerc);
    const memory = parseBytes(String(row.MemUsage || '').split('/')[0]);
    if (!containers.includes(row.Name) || cpuPercent === null || memory === null) {
      throw new Error('invalid Docker resource sample');
    }
    samples.push({ at: Date.now(), name: row.Name, cpuPercent, memory });
  }
  const top = spawnSync('docker', ['top', RSCTF, '-eLo', 'tid='], { encoding: 'utf8' });
  if (top.status !== 0) throw new Error(`docker top failed: ${(top.stderr || top.stdout || '').trim()}`);
  const tasks = top.stdout.split('\n').filter((line) => /^\s*\d+\s*$/.test(line)).length;
  if (tasks < 1) throw new Error('runtime task/thread sample is empty');
  samples.push({ at: Date.now(), name: `${RSCTF}:tasks`, tasks });
  databaseSamples.push({ at: Date.now(), ...databaseSample() });
};

const validateDatabaseBounds = () => {
  if (databaseSamples.length < 2) throw new Error('insufficient PostgreSQL activity samples');
  const peak = (key) => Math.max(...databaseSamples.map((row) => row[key]));
  const peakConnections = peak('poolConnections');
  const peakActiveConnections = peak('activeConnections');
  const peakIdleInTransaction = peak('idleInTransactionConnections');
  const peakWaitingConnections = peak('waitingConnections');
  const longestTransactionSeconds = peak('longestTransactionSeconds');
  if (peakConnections > limits.pgConnections) {
    throw new Error(`PostgreSQL pool connections peaked at ${peakConnections}`);
  }
  if (peakActiveConnections > limits.pgActiveConnections) {
    throw new Error(`PostgreSQL active connections peaked at ${peakActiveConnections}`);
  }
  if (peakIdleInTransaction > limits.pgIdleInTransaction) {
    throw new Error(`PostgreSQL idle-in-transaction connections peaked at ${peakIdleInTransaction}`);
  }
  if (peakWaitingConnections > limits.pgWaitingConnections) {
    throw new Error(`PostgreSQL waiting connections peaked at ${peakWaitingConnections}`);
  }
  if (longestTransactionSeconds > limits.pgLongestTransactionSeconds) {
    throw new Error(`PostgreSQL longest transaction reached ${longestTransactionSeconds.toFixed(3)}s`);
  }
  const first = databaseSamples[0];
  const last = databaseSamples.at(-1);
  const blockReadDelta = last.blockReads - first.blockReads;
  const tempByteDelta = last.tempBytes - first.tempBytes;
  if (blockReadDelta < 0 || blockReadDelta > limits.pgBlockReads) {
    throw new Error(`PostgreSQL block-read delta was ${blockReadDelta}`);
  }
  if (tempByteDelta < 0 || tempByteDelta > limits.pgTempMiB * 1024 * 1024) {
    throw new Error(`PostgreSQL temp I/O delta was ${tempByteDelta}`);
  }
  return {
    peakConnections,
    peakActiveConnections,
    peakIdleInTransaction,
    peakWaitingConnections,
    longestTransactionSeconds,
    blockReadDelta,
    tempByteDelta,
  };
};

const exactHealth = async (stage) => {
  const response = await fetch(new URL('/healthz', target), { signal: AbortSignal.timeout(3_000) });
  const body = await response.text();
  if (response.status !== 200 || body !== 'ok') {
    throw new Error(`${stage} healthz failed: HTTP ${response.status} ${JSON.stringify(body)}`);
  }
};

const slowSockets = [];
let connectedSlowSockets = 0;
const startSlowSockets = () => {
  if (tcpPort === null) return;
  for (let index = 0; index < tcpConnections; index += 1) {
    const socket = net.createConnection({ host: target.hostname, port: tcpPort });
    socket.once('connect', () => {
      connectedSlowSockets += 1;
    });
    socket.on('error', () => {});
    slowSockets.push(socket);
  }
};

const startBucketMs = Number(
  sql("SELECT (EXTRACT(EPOCH FROM date_trunc('minute', clock_timestamp())) * 1000)::BIGINT"),
);
if (!Number.isSafeInteger(startBucketMs) || startBucketMs < 0) {
  throw new Error('invalid honeypot aggregate bucket checkpoint');
}
const quotedBaits = loadBaits.map((bait) => `'${bait.replaceAll("'", "''")}'`).join(',');
const aggregateSnapshot = () => {
  const raw = sql(
    `SELECT COALESCE(json_agg(json_build_object(` +
      `'bucketStartMs', (EXTRACT(EPOCH FROM bucket_start_utc) * 1000)::BIGINT, ` +
      `'bait', bait, 'sourceHash', encode(source_hash, 'hex'), ` +
      `'hits', hit_count, 'baitBytes', OCTET_LENGTH(bait), ` +
      `'userAgentBytes', COALESCE(OCTET_LENGTH(user_agent), 0)` +
      `) ORDER BY bucket_start_utc, bait, source_hash), '[]'::json)::text ` +
      `FROM "HoneypotHits" WHERE bucket_start_utc >= to_timestamp(${startBucketMs} / 1000.0) ` +
      `AND source_hash IS NOT NULL AND bait IN (${quotedBaits})`,
  );
  const rows = JSON.parse(raw || '[]');
  if (!Array.isArray(rows)) throw new Error('invalid honeypot aggregate snapshot');
  return rows.map((row) => {
    const values = [row.bucketStartMs, row.hits, row.baitBytes, row.userAgentBytes];
    if (
      !values.every((value) => Number.isSafeInteger(Number(value)) && Number(value) >= 0) ||
      typeof row.bait !== 'string' ||
      !/^[0-9a-f]{64}$/.test(String(row.sourceHash))
    ) {
      throw new Error('invalid honeypot aggregate snapshot row');
    }
    return {
      ...row,
      bucketStartMs: Number(row.bucketStartMs),
      hits: Number(row.hits),
      baitBytes: Number(row.baitBytes),
      userAgentBytes: Number(row.userAgentBytes),
      sourceHash: String(row.sourceHash),
    };
  });
};
const aggregateKey = (row) => `${row.bucketStartMs}|${row.bait}|${row.sourceHash}`;

await exactHealth('pre-load');
const beforeRows = aggregateSnapshot();
const beforeHits = new Map(beforeRows.map((row) => [aggregateKey(row), row.hits]));
sample();
startSlowSockets();

let sampler;
let child;
let status = 1;
try {
  sampler = setInterval(() => {
    try {
      sample();
    } catch (error) {
      samplingErrors.push(error instanceof Error ? error.message : String(error));
    }
  }, 1_000);
  const args = ['run'];
  if (process.env.SUMMARY_JSON) args.push('--summary-export', resolve(process.env.SUMMARY_JSON));
  args.push(new URL('./k6/honeypot-bounds.js', import.meta.url).pathname);
  child = spawn('k6', args, {
    stdio: 'inherit',
    env: { ...process.env, TARGET: target.origin, RATE: String(rate), VUS: String(vus), SOURCE_COUNT: String(sourceCount), DURATION: duration },
  });
  status = await new Promise((resolveStatus, rejectStatus) => {
    child.once('error', rejectStatus);
    child.once('close', (code) => resolveStatus(code ?? 1));
  });
  await new Promise((resolveWait) => setTimeout(resolveWait, 3_500));
  if (tcpPort !== null && connectedSlowSockets < 1) {
    throw new Error('TCP slow-loris gate did not establish any honeypot connection');
  }
  for (const socket of slowSockets) {
    if (!socket.destroyed) throw new Error('TCP slow-loris socket outlived the absolute connection deadline');
  }
  await exactHealth('post-load');
  clearInterval(sampler);
  sampler = undefined;
  sample();
  if (samplingErrors.length > 0) {
    throw new Error(`resource sampling failed: ${samplingErrors.join('; ')}`);
  }

  const afterRows = aggregateSnapshot();
  let newRows = 0;
  let hitDelta = 0;
  let maxBaitBytes = 0;
  let maxUserAgentBytes = 0;
  const changedSources = new Set();
  for (const row of afterRows) {
    const key = aggregateKey(row);
    const priorHits = beforeHits.get(key) || 0;
    if (!beforeHits.has(key)) newRows += 1;
    const delta = row.hits - priorHits;
    if (delta > 0) {
      hitDelta += delta;
      changedSources.add(row.sourceHash);
    }
    maxBaitBytes = Math.max(maxBaitBytes, row.baitBytes);
    maxUserAgentBytes = Math.max(maxUserAgentBytes, row.userAgentBytes);
  }
  if (hitDelta < 1) throw new Error('honeypot aggregate hit counts did not advance');
  if (newRows > maxRows) throw new Error(`honeypot aggregate rows exceeded bound: ${newRows} > ${maxRows}`);
  if (changedSources.size < Math.min(sourceCount, 2)) {
    throw new Error(
      'distinct trusted-proxy source identities were not persisted; target may not trust X-Forwarded-For',
    );
  }
  if (maxBaitBytes > 128 || maxUserAgentBytes > 256) {
    throw new Error(`stored honeypot fields exceeded bounds: ${maxBaitBytes}|${maxUserAgentBytes}`);
  }

  const cpuPeaks = [];
  for (const name of containers) {
    const rows = samples.filter((entry) => entry.name === name);
    if (rows.length < 2) throw new Error(`insufficient resource samples for ${name}`);
    const peakCpuPercent = Math.max(...rows.map((entry) => entry.cpuPercent));
    if (peakCpuPercent > limits.cpuPercent) {
      throw new Error(`${name} CPU peaked at ${peakCpuPercent}%`);
    }
    cpuPeaks.push(`${name}:${peakCpuPercent.toFixed(1)}%`);
    const delta = Math.max(...rows.map((entry) => entry.memory)) - rows[0].memory;
    if (delta > 128 * 1024 * 1024) throw new Error(`${name} memory grew by more than 128 MiB`);
  }
  const taskRows = samples.filter((entry) => entry.name === `${RSCTF}:tasks`);
  const taskDelta = Math.max(...taskRows.map((entry) => entry.tasks)) - taskRows[0].tasks;
  if (taskDelta > 160) throw new Error(`runtime tasks/threads grew by ${taskDelta}`);
  const database = validateDatabaseBounds();
  console.log(
    `honeypot_new_rows=${newRows} estimated_hit_delta=${hitDelta} distinct_sources=${changedSources.size} ` +
      `max_rows=${maxRows} task_delta=${taskDelta} cpu_peaks=${cpuPeaks.join(',')} ` +
      `pg_peak_connections=${database.peakConnections} pg_peak_active=${database.peakActiveConnections} ` +
      `pg_peak_idle_in_transaction=${database.peakIdleInTransaction} ` +
      `pg_peak_waiting=${database.peakWaitingConnections} ` +
      `pg_longest_transaction_s=${database.longestTransactionSeconds.toFixed(3)} ` +
      `pg_block_reads=${database.blockReadDelta} pg_temp_bytes=${database.tempByteDelta}`,
  );
} finally {
  if (sampler) clearInterval(sampler);
  if (child && child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
  for (const socket of slowSockets) socket.destroy();
}
process.exit(status);
