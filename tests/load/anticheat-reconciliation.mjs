// Destructive-to-fixture fixed-rate gate for incremental anti-cheat scheduling.
import { randomUUID } from 'node:crypto';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { mintJwt, PG, RSCTF, sql, TARGET } from './lib.mjs';

const integer = (value, name, minimum, maximum) => {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${name} must be an integer in ${minimum}..${maximum}`);
  }
  return parsed;
};
const sleep = (milliseconds) => new Promise((resolveWait) => setTimeout(resolveWait, milliseconds));

const game = integer(process.env.ANTICHEAT_RECONCILIATION_GAME || process.env.GAME, 'GAME', 1, 2_147_483_647);
if (process.env.ANTICHEAT_RECONCILIATION_STRESS_ACK !== `game:${game}`) {
  throw new Error(`ANTICHEAT_RECONCILIATION_STRESS_ACK=game:${game} is required; this gate advances one disposable reconciliation generation`);
}
const target = new URL(TARGET);
if (
  !['127.0.0.1', 'localhost', '::1'].includes(target.hostname) &&
  process.env.ALLOW_REMOTE_ANTICHEAT_RECONCILIATION_STRESS !== target.origin
) {
  throw new Error(`remote TARGET requires ALLOW_REMOTE_ANTICHEAT_RECONCILIATION_STRESS=${target.origin}`);
}
const rate = integer(process.env.RATE || 20, 'RATE', 1, 1_000);
const vus = integer(process.env.VUS || 32, 'VUS', 2, 1_024);
const manualRequests = integer(process.env.MANUAL_REQUESTS || 16, 'MANUAL_REQUESTS', 2, 128);
const minimumHistory = integer(process.env.MIN_ANTICHEAT_HISTORY || 5_000, 'MIN_ANTICHEAT_HISTORY', 1, 100_000_000);
const duration = String(process.env.DURATION || '65s');
const durationMatch = duration.match(/^([1-9]\d*)(s|m)$/);
const durationSeconds = durationMatch ? Number(durationMatch[1]) * (durationMatch[2] === 'm' ? 60 : 1) : 0;
if (durationSeconds < 35 || durationSeconds > 600) throw new Error('DURATION must be between 35s and 10m');
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

const resourceSamples = [];
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
const sampleResources = () => {
  const stats = spawnSync('docker', ['stats', '--no-stream', '--format', '{{json .}}', RSCTF, PG], {
    encoding: 'utf8',
  });
  if (stats.status !== 0) throw new Error(`docker stats failed: ${(stats.stderr || stats.stdout || '').trim()}`);
  for (const line of stats.stdout.split('\n').filter(Boolean)) {
    const row = JSON.parse(line);
    const cpuPercent = parsePercent(row.CPUPerc);
    const memory = parseBytes(String(row.MemUsage || '').split('/')[0]);
    if (![RSCTF, PG].includes(row.Name) || cpuPercent === null || memory === null) {
      throw new Error('invalid Docker resource sample');
    }
    resourceSamples.push({ at: Date.now(), name: row.Name, cpuPercent, memory });
  }
  const top = spawnSync('docker', ['top', RSCTF, '-eLo', 'tid='], { encoding: 'utf8' });
  if (top.status !== 0) throw new Error(`docker top failed: ${(top.stderr || top.stdout || '').trim()}`);
  const tasks = top.stdout.split('\n').filter((line) => /^\s*\d+\s*$/.test(line)).length;
  if (tasks < 1) throw new Error('runtime task/thread sample is empty');
  resourceSamples.push({ at: Date.now(), name: `${RSCTF}:tasks`, tasks });
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

const state = () => {
  const raw = sql(
    `SELECT json_build_object(` +
      `'desired', queue.desired_generation, 'applied', queue.applied_generation, ` +
      `'dirtySources', (SELECT COUNT(*) FROM "AntiCheatReconciliationSources" source ` +
      `WHERE source.game_id=queue.game_id AND source.dirty_version>source.applied_version), ` +
      `'attempts', reconciliation.attempts, ` +
      `'lastStarted', COALESCE(queue.last_started_at_utc::text, ''), ` +
      `'activeJobs', (SELECT COUNT(*) FROM "ControlPlaneJobs" job WHERE job.kind='SecurityDerivation' ` +
      `AND job.game_id=queue.game_id AND job.status IN (0,1)), ` +
      `'jobCount', (SELECT COUNT(*) FROM "ControlPlaneJobs" job WHERE job.kind='SecurityDerivation' ` +
      `AND job.game_id=queue.game_id), ` +
      `'aliasCount', (SELECT COUNT(*) FROM "ControlPlaneJobOperations" operation JOIN "ControlPlaneJobs" job ` +
      `ON job.id=operation.job_id WHERE job.kind='SecurityDerivation' AND job.game_id=queue.game_id)` +
      `)::text FROM "AntiCheatReconciliationQueue" queue ` +
      `JOIN "SuspicionReconciliationState" reconciliation ON reconciliation.game_id=queue.game_id ` +
      `JOIN "Games" game ON game.id=queue.game_id ` +
      `WHERE queue.game_id=${game} AND game.start_time_utc<=clock_timestamp() ` +
      `AND clock_timestamp()<game.end_time_utc AND reconciliation.evidence_closed_at_utc IS NULL ` +
      `AND reconciliation.sealed_at_utc IS NULL`,
  );
  if (!raw) throw new Error(`game ${game} must be active with open anti-cheat evidence intake`);
  const value = JSON.parse(raw);
  for (const key of ['desired', 'applied', 'dirtySources', 'attempts', 'activeJobs', 'jobCount', 'aliasCount']) {
    value[key] = Number(value[key]);
    if (!Number.isSafeInteger(value[key]) || value[key] < 0) throw new Error(`invalid reconciliation ${key}`);
  }
  return value;
};

const waitForClean = async (timeoutMs = 90_000) => {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const current = state();
    if (current.desired === current.applied && current.dirtySources === 0 && current.activeJobs === 0) return current;
    if (Date.now() >= deadline) throw new Error(`anti-cheat did not become idle: ${JSON.stringify(current)}`);
    await sleep(1_000);
  }
};

const history = Number(
  sql(
    `SELECT (` +
      `(SELECT COUNT(*) FROM "SuspicionEvaluationOutbox" WHERE game_id=${game}) + ` +
      `(SELECT COUNT(*) FROM "IdentityObservations" WHERE game_id=${game}) + ` +
      `(SELECT COUNT(*) FROM "SuspicionEvents" WHERE game_id=${game}) + ` +
      `(SELECT COUNT(*) FROM "CheatInfo" WHERE game_id=${game}) + ` +
      `(SELECT COUNT(*) FROM "VpnDnsProviderBuckets" WHERE game_id=${game}) + ` +
      `(SELECT COUNT(*) FROM "VpnPeerNetworkObservations" WHERE game_id=${game}) + ` +
      `(SELECT COUNT(*) FROM "VpnFlagTransportEvents" WHERE game_id=${game})` +
      `)::BIGINT`,
  ),
);
if (!Number.isSafeInteger(history) || history < minimumHistory) {
  throw new Error(`anti-cheat fixture history is too small: ${history} < ${minimumHistory}`);
}

const adminRows = sql(
  `SELECT id::text || '|' || security_stamp FROM "AspNetUsers" ` +
    `WHERE role=3 AND security_stamp IS NOT NULL ORDER BY id LIMIT 2`,
).split('\n').filter(Boolean);
if (adminRows.length !== 2) throw new Error('two disposable Admin accounts are required');
const adminTokens = adminRows.map((row) => {
  const [id, stamp] = row.split('|');
  return mintJwt(id, stamp, 3);
});

const postManual = async (operationId, token) => {
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const response = await fetch(new URL(`/api/admin/games/${game}/anti-cheat/derive`, target), {
      method: 'POST',
      headers: { Authorization: `Bearer ${token}`, 'Idempotency-Key': operationId },
      signal: AbortSignal.timeout(5_000),
    });
    const text = await response.text();
    if (response.status === 202) {
      if (Buffer.byteLength(text) > 64 * 1024) throw new Error('manual derivation response exceeded 64 KiB');
      const job = JSON.parse(text);
      if (!job || !/^[0-9a-f-]{36}$/i.test(String(job.id))) throw new Error('invalid derivation job response');
      return String(job.id).toLowerCase();
    }
    if (![429, 503].includes(response.status) || attempt === 3) {
      throw new Error(`manual derivation failed: HTTP ${response.status} ${text.slice(0, 300)}`);
    }
    const retrySeconds = Math.min(2, Math.max(1, Number(response.headers.get('retry-after')) || 1));
    await sleep(retrySeconds * 1_000);
  }
  throw new Error('unreachable manual derivation retry state');
};

await exactHealth('pre-manual');
const beforeManual = await waitForClean();
sql(
  `UPDATE "AntiCheatReconciliationQueue" SET desired_generation=desired_generation+1, ` +
    `available_at_utc=clock_timestamp(), updated_at_utc=clock_timestamp() WHERE game_id=${game}`,
);
const operations = Array.from({ length: manualRequests }, () => randomUUID());
const jobIds = await Promise.all(
  operations.map((operation, index) => postManual(operation, adminTokens[index % adminTokens.length])),
);
if (new Set(jobIds).size !== 1) throw new Error(`manual/scheduled requests did not coalesce: ${new Set(jobIds).size} jobs`);
const operationList = operations.map((operation) => `'${operation}'::uuid`).join(',');
const aliased = Number(
  sql(
    `SELECT COUNT(*) FROM "ControlPlaneJobOperations" WHERE job_id='${jobIds[0]}'::uuid ` +
      `AND operation_id IN (${operationList})`,
  ),
);
if (aliased !== operations.length) throw new Error(`only ${aliased}/${operations.length} manual operations were aliased`);
const afterManual = await waitForClean();
if (afterManual.attempts !== beforeManual.attempts + 1) {
  throw new Error(`one dirty generation ran ${afterManual.attempts - beforeManual.attempts} effective passes`);
}

sampleResources();
const idleBaseline = state();
const fixtureDirectory = mkdtempSync(join(tmpdir(), 'rsctf-anticheat-reconcile-'));
const tokensFile = join(fixtureDirectory, 'tokens.json');
writeFileSync(tokensFile, JSON.stringify(adminTokens), { mode: 0o600 });
let sampler;
let child;
let status = 1;
try {
  sampler = setInterval(() => {
    try {
      sampleResources();
    } catch (error) {
      samplingErrors.push(error instanceof Error ? error.message : String(error));
    }
  }, 1_000);
  const args = ['run'];
  if (process.env.SUMMARY_JSON) args.push('--summary-export', resolve(process.env.SUMMARY_JSON));
  args.push(new URL('./k6/anticheat-reconciliation.js', import.meta.url).pathname);
  child = spawn('k6', args, {
    stdio: 'inherit',
    env: {
      ...process.env,
      TARGET: target.origin,
      GAME: String(game),
      RATE: String(rate),
      VUS: String(vus),
      DURATION: duration,
      TOKENS_FILE: tokensFile,
    },
  });
  status = await new Promise((resolveStatus, rejectStatus) => {
    child.once('error', rejectStatus);
    child.once('close', (code) => resolveStatus(code ?? 1));
  });
  await sleep(2_000);
  await exactHealth('post-load');
  const idleAfter = state();
  for (const key of ['desired', 'applied', 'dirtySources', 'attempts', 'activeJobs', 'jobCount', 'aliasCount']) {
    if (idleAfter[key] !== idleBaseline[key]) {
      throw new Error(`idle reconciliation changed ${key}: ${idleBaseline[key]} -> ${idleAfter[key]}`);
    }
  }
  if (idleAfter.lastStarted !== idleBaseline.lastStarted) throw new Error('idle reconciliation started another pass');
  clearInterval(sampler);
  sampler = undefined;
  sampleResources();
  if (samplingErrors.length > 0) {
    throw new Error(`resource sampling failed: ${samplingErrors.join('; ')}`);
  }
  const cpuPeaks = [];
  for (const name of [RSCTF, PG]) {
    const rows = resourceSamples.filter((row) => row.name === name);
    if (rows.length < 2) throw new Error(`insufficient resource samples for ${name}`);
    const peakCpuPercent = Math.max(...rows.map((row) => row.cpuPercent));
    if (peakCpuPercent > limits.cpuPercent) {
      throw new Error(`${name} CPU peaked at ${peakCpuPercent}%`);
    }
    cpuPeaks.push(`${name}:${peakCpuPercent.toFixed(1)}%`);
    const delta = Math.max(...rows.map((row) => row.memory)) - rows[0].memory;
    if (delta > 256 * 1024 * 1024) throw new Error(`${name} memory grew by more than 256 MiB`);
  }
  const taskRows = resourceSamples.filter((row) => row.name === `${RSCTF}:tasks`);
  const taskDelta = Math.max(...taskRows.map((row) => row.tasks)) - taskRows[0].tasks;
  if (taskDelta > 32) throw new Error(`runtime tasks/threads grew by ${taskDelta}`);
  const database = validateDatabaseBounds();
  console.log(
    `anticheat_history=${history} coalesced_operations=${operations.length} idle_passes=0 ` +
      `task_delta=${taskDelta} cpu_peaks=${cpuPeaks.join(',')} ` +
      `pg_peak_connections=${database.peakConnections} pg_peak_active=${database.peakActiveConnections} ` +
      `pg_peak_idle_in_transaction=${database.peakIdleInTransaction} ` +
      `pg_peak_waiting=${database.peakWaitingConnections} ` +
      `pg_longest_transaction_s=${database.longestTransactionSeconds.toFixed(3)} ` +
      `pg_block_reads=${database.blockReadDelta} pg_temp_bytes=${database.tempByteDelta}`,
  );
} finally {
  if (sampler) clearInterval(sampler);
  if (child && child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
  rmSync(fixtureDirectory, { recursive: true, force: true });
}
process.exit(status);
