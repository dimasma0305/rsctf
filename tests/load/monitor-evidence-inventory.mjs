// Read-only fixed-rate gate for the bounded traffic and anti-cheat monitor surfaces.
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { mintJwt, PG, RSCTF, sql, TARGET } from './lib.mjs';

const boundedInteger = (value, name, minimum, maximum) => {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${name} must be an integer in ${minimum}..${maximum}`);
  }
  return parsed;
};

const game = boundedInteger(
  process.env.MONITOR_EVIDENCE_GAME || process.env.GAME,
  'MONITOR_EVIDENCE_GAME',
  1,
  2_147_483_647,
);
const target = new URL(TARGET);
const rate = boundedInteger(process.env.RATE || 4, 'RATE', 1, 200);
const vus = boundedInteger(process.env.VUS || 32, 'VUS', 4, 512);
const duration = String(process.env.DURATION || '30s');
const durationMatch = duration.match(/^([1-9]\d*)(s|m)$/);
const durationSeconds = durationMatch ? Number(durationMatch[1]) * (durationMatch[2] === 'm' ? 60 : 1) : 0;
if (durationSeconds < 1 || durationSeconds > 600) throw new Error('DURATION must be between 1s and 10m');
if (
  !['127.0.0.1', 'localhost', '::1'].includes(target.hostname) &&
  process.env.ALLOW_REMOTE_MONITOR_EVIDENCE_STRESS !== target.origin
) {
  throw new Error(`remote TARGET requires ALLOW_REMOTE_MONITOR_EVIDENCE_STRESS=${target.origin}`);
}
const minimums = {
  challenges: boundedInteger(process.env.MIN_CAPTURE_CHALLENGES || 20, 'MIN_CAPTURE_CHALLENGES', 1, 100_000),
  buckets: boundedInteger(process.env.MIN_CAPTURE_BUCKETS || 500, 'MIN_CAPTURE_BUCKETS', 1, 1_000_000),
  files: boundedInteger(process.env.MIN_CAPTURE_FILES || 5_000, 'MIN_CAPTURE_FILES', 1, 10_000_000),
  incidents: boundedInteger(process.env.MIN_CHEAT_INCIDENTS || 1_000, 'MIN_CHEAT_INCIDENTS', 1, 100_000),
  events: boundedInteger(process.env.MIN_SUSPICION_EVENTS || 5_000, 'MIN_SUSPICION_EVENTS', 1, 1_000_000),
};
const limits = {
  memoryMiB: boundedInteger(process.env.MAX_MEMORY_DELTA_MIB || 256, 'MAX_MEMORY_DELTA_MIB', 1, 16_384),
  tasks: boundedInteger(process.env.MAX_TASK_DELTA || 32, 'MAX_TASK_DELTA', 1, 4_096),
  blockIoMiB: boundedInteger(process.env.MAX_BLOCK_IO_DELTA_MIB || 1_024, 'MAX_BLOCK_IO_DELTA_MIB', 1, 65_536),
  pgBlockReads: boundedInteger(
    process.env.MAX_PG_BLOCK_READ_DELTA || 100_000,
    'MAX_PG_BLOCK_READ_DELTA',
    1,
    10_000_000,
  ),
  pgTempMiB: boundedInteger(process.env.MAX_PG_TEMP_DELTA_MIB || 64, 'MAX_PG_TEMP_DELTA_MIB', 1, 65_536),
};
const resourceContainers = String(process.env.MONITOR_EVIDENCE_RESOURCE_CONTAINERS || `${RSCTF},${PG}`)
  .split(',')
  .map((value) => value.trim())
  .filter(Boolean);
if (
  resourceContainers.length < 1 ||
  resourceContainers.length > 16 ||
  !resourceContainers.every((name) => /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(name))
) {
  throw new Error('MONITOR_EVIDENCE_RESOURCE_CONTAINERS requires 1..16 bounded Docker container names');
}
const captureRoot = String(process.env.MONITOR_EVIDENCE_CAPTURE_ROOT || '/data/files/capture');
if (
  !captureRoot.startsWith('/') ||
  captureRoot.length > 512 ||
  captureRoot.split('/').some((part) => part === '..') ||
  /[\0\r\n]/.test(captureRoot)
) {
  throw new Error('MONITOR_EVIDENCE_CAPTURE_ROOT must be a bounded absolute container path');
}
const FLOW_SEED_CANDIDATE_LIMIT = 8;
const FLOW_SEED_MAX_CAPTURE_BYTES = 256 * 1024 * 1024;
const FLOW_SEED_MAX_RESPONSE_BYTES = 128 * 1024;
const FLOW_SEED_TIMEOUT_MS = 60_000;

const parseBytes = (value) => {
  const match = String(value)
    .trim()
    .match(/^([0-9.]+)([kmgt]?i?b)$/i);
  if (!match) return null;
  const powers = {
    b: 0,
    kb: 1,
    kib: 1,
    mb: 2,
    mib: 2,
    gb: 3,
    gib: 3,
    tb: 4,
    tib: 4,
  };
  return Number(match[1]) * 1024 ** (powers[match[2].toLowerCase()] ?? 0);
};

const databaseIo = () => {
  const fields = sql(
    `SELECT blks_read::text || '|' || temp_bytes::text FROM pg_stat_database WHERE datname=current_database()`,
  )
    .split('|')
    .map(Number);
  if (fields.length !== 2 || fields.some((value) => !Number.isSafeInteger(value) || value < 0)) {
    throw new Error('PostgreSQL I/O counters are unavailable');
  }
  return { blockReads: fields[0], tempBytes: fields[1] };
};

const resourceSamples = [];
const samplingErrors = [];
const sampleResources = () => {
  const sampled = spawnSync('docker', ['stats', '--no-stream', '--format', '{{json .}}', ...resourceContainers], {
    encoding: 'utf8',
  });
  if (sampled.status !== 0) {
    throw new Error(`docker stats failed: ${(sampled.stderr || sampled.stdout || '').trim()}`);
  }
  const seen = new Set();
  for (const line of sampled.stdout.split('\n').filter(Boolean)) {
    const row = JSON.parse(line);
    if (!resourceContainers.includes(row.Name)) continue;
    const memoryBytes = parseBytes(String(row.MemUsage || '').split('/')[0]);
    const blockParts = String(row.BlockIO || '')
      .split('/')
      .map(parseBytes);
    if (memoryBytes === null || blockParts.length !== 2 || blockParts.some((value) => value === null)) {
      throw new Error(`invalid Docker resource row for ${String(row.Name || 'unknown')}`);
    }
    seen.add(row.Name);
    resourceSamples.push({
      at: Date.now(),
      name: row.Name,
      memoryBytes,
      blockIoBytes: blockParts[0] + blockParts[1],
    });
  }
  const missing = resourceContainers.filter((name) => !seen.has(name));
  if (missing.length > 0) throw new Error(`docker stats omitted: ${missing.join(', ')}`);

  const top = spawnSync('docker', ['top', RSCTF, '-eLo', 'tid='], {
    encoding: 'utf8',
  });
  if (top.status !== 0) throw new Error(`docker top failed: ${(top.stderr || top.stdout || '').trim()}`);
  const tasks = top.stdout
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => /^\d+$/.test(line)).length;
  if (tasks < 1) throw new Error('runtime task/thread sample is empty');
  resourceSamples.push({ at: Date.now(), name: `${RSCTF}:tasks`, tasks });
};

const validateResourceBounds = (beforeIo, afterIo) => {
  const summaries = resourceContainers.map((name) => {
    const rows = resourceSamples.filter((sample) => sample.name === name);
    if (rows.length < 2) throw new Error(`insufficient resource samples for ${name}`);
    const memoryDelta = Math.max(...rows.map((row) => row.memoryBytes)) - rows[0].memoryBytes;
    const blockIoDelta = Math.max(...rows.map((row) => row.blockIoBytes)) - rows[0].blockIoBytes;
    if (memoryDelta > limits.memoryMiB * 1024 * 1024) {
      throw new Error(`${name} memory grew by ${(memoryDelta / 1024 / 1024).toFixed(1)} MiB`);
    }
    if (blockIoDelta > limits.blockIoMiB * 1024 * 1024) {
      throw new Error(`${name} block I/O grew by ${(blockIoDelta / 1024 / 1024).toFixed(1)} MiB`);
    }
    return { name, memoryDelta, blockIoDelta };
  });
  const taskRows = resourceSamples.filter((sample) => sample.name === `${RSCTF}:tasks`);
  if (taskRows.length < 2) throw new Error('insufficient runtime task/thread samples');
  const taskDelta = Math.max(...taskRows.map((row) => row.tasks)) - taskRows[0].tasks;
  if (taskDelta > limits.tasks) throw new Error(`runtime tasks/threads grew by ${taskDelta}`);
  const pgBlockReadDelta = afterIo.blockReads - beforeIo.blockReads;
  const pgTempDelta = afterIo.tempBytes - beforeIo.tempBytes;
  if (pgBlockReadDelta < 0 || pgBlockReadDelta > limits.pgBlockReads) {
    throw new Error(`PostgreSQL block reads grew by ${pgBlockReadDelta}`);
  }
  if (pgTempDelta < 0 || pgTempDelta > limits.pgTempMiB * 1024 * 1024) {
    throw new Error(`PostgreSQL temp I/O grew by ${(pgTempDelta / 1024 / 1024).toFixed(1)} MiB`);
  }
  return { summaries, taskDelta, pgBlockReadDelta, pgTempDelta };
};

async function exactHealth(stage) {
  const response = await fetch(new URL('/healthz', TARGET), {
    signal: AbortSignal.timeout(3_000),
  });
  const body = await response.text();
  if (response.status !== 200 || body !== 'ok') {
    throw new Error(`${stage} healthz failed: HTTP ${response.status} ${JSON.stringify(body)}`);
  }
}

await exactHealth('pre-load');

const counts = sql(
  `SELECT ` +
    `(SELECT COUNT(*) FROM "GameChallenges" WHERE game_id=${game} AND enable_traffic_capture=TRUE)::text || '|' || ` +
    `(SELECT COUNT(*) FROM "TrafficCaptureBuckets" bucket JOIN "GameChallenges" challenge ` +
    `ON challenge.id=bucket.challenge_id WHERE challenge.game_id=${game} AND bucket.file_count>0)::text || '|' || ` +
    `(SELECT COUNT(*) FROM "TrafficCaptureFiles" file JOIN "GameChallenges" challenge ` +
    `ON challenge.id=file.challenge_id WHERE challenge.game_id=${game})::text || '|' || ` +
    `(SELECT COUNT(*) FROM "CheatInfo" WHERE game_id=${game})::text || '|' || ` +
    `(SELECT COUNT(*) FROM "SuspicionEvents" WHERE game_id=${game})::text`,
)
  .split('|')
  .map(Number);
const labels = ['challenges', 'buckets', 'files', 'incidents', 'events'];
for (let index = 0; index < labels.length; index += 1) {
  const label = labels[index];
  if (!Number.isSafeInteger(counts[index]) || counts[index] < minimums[label]) {
    throw new Error(
      `monitor evidence/inventory fixture is too small: ${label}=${counts[index]}, required=${minimums[label]}`,
    );
  }
}

// PostgreSQL is the serving index, but the acceptance fixture must also contain
// the authoritative regular PCAPs. GNU find emits one byte per file so even the
// configured ten-million-file ceiling stays inside this explicit buffer bound.
const filesystemInventory = spawnSync(
  'docker',
  ['exec', RSCTF, 'find', captureRoot, '-type', 'f', '-iname', '*.pcap', '-printf', '.'],
  { encoding: 'buffer', maxBuffer: 16 * 1024 * 1024 },
);
if (filesystemInventory.status !== 0) {
  throw new Error(
    `capture filesystem inventory failed: ${String(filesystemInventory.stderr || filesystemInventory.stdout || '').trim()}`,
  );
}
const filesystemFiles = filesystemInventory.stdout.length;
if (filesystemFiles < minimums.files) {
  throw new Error(`capture filesystem fixture is too small: files=${filesystemFiles}, required=${minimums.files}`);
}

const bucket = sql(
  `SELECT bucket.challenge_id::text || '|' || bucket.participation_id::text ` +
    `FROM "TrafficCaptureBuckets" bucket ` +
    `JOIN "GameChallenges" challenge ON challenge.id=bucket.challenge_id ` +
    `JOIN "Participations" participation ON participation.id=bucket.participation_id ` +
    `AND participation.game_id=challenge.game_id ` +
    `WHERE challenge.game_id=${game} AND bucket.file_count>0 ` +
    `ORDER BY bucket.file_count DESC, bucket.challenge_id, bucket.participation_id LIMIT 1`,
)
  .split('|')
  .map(Number);
if (bucket.length !== 2 || bucket.some((value) => !Number.isSafeInteger(value) || value <= 0)) {
  throw new Error(`game ${game} needs one indexed capture bucket`);
}

const flowCandidates = JSON.parse(
  sql(
    `SELECT COALESCE(json_agg(row_to_json(candidate)), '[]'::json)::text FROM (` +
      `SELECT file.challenge_id AS "challengeId", file.participation_id AS "participationId", ` +
      `file.file_name AS "filename", file.size_bytes AS "sizeBytes" ` +
      `FROM "TrafficCaptureFiles" file ` +
      `JOIN "GameChallenges" challenge ON challenge.id=file.challenge_id ` +
      `JOIN "Participations" participation ON participation.id=file.participation_id ` +
      `AND participation.game_id=challenge.game_id ` +
      `WHERE challenge.game_id=${game} AND file.size_bytes>=24 ` +
      `AND file.size_bytes<=${FLOW_SEED_MAX_CAPTURE_BYTES} ` +
      `ORDER BY file.size_bytes, file.modified_at_utc, file.file_name ` +
      `LIMIT ${FLOW_SEED_CANDIDATE_LIMIT}` +
      `) candidate`,
  ),
);
if (
  !Array.isArray(flowCandidates) ||
  flowCandidates.length < 1 ||
  flowCandidates.length > FLOW_SEED_CANDIDATE_LIMIT ||
  !flowCandidates.every(
    (candidate) =>
      candidate &&
      Number.isSafeInteger(candidate.challengeId) &&
      candidate.challengeId > 0 &&
      Number.isSafeInteger(candidate.participationId) &&
      candidate.participationId > 0 &&
      Number.isSafeInteger(candidate.sizeBytes) &&
      candidate.sizeBytes >= 24 &&
      candidate.sizeBytes <= FLOW_SEED_MAX_CAPTURE_BYTES &&
      typeof candidate.filename === 'string' &&
      candidate.filename.length <= 255 &&
      /^[^/\\\r\n]+\.pcap$/i.test(candidate.filename),
  )
) {
  throw new Error(`game ${game} needs a bounded indexed PCAP candidate`);
}

const eventId = Number(sql(`SELECT id FROM "SuspicionEvents" WHERE game_id=${game} ORDER BY id DESC LIMIT 1`));
if (!Number.isSafeInteger(eventId) || eventId <= 0) throw new Error(`game ${game} needs one suspicion event`);

const pair = sql(
  `SELECT participation.id FROM "Participations" participation ` +
    `WHERE participation.game_id=${game} AND EXISTS (` +
    `SELECT 1 FROM "FirstSolves" solve WHERE solve.participation_id=participation.id` +
    `) ORDER BY participation.id LIMIT 2`,
)
  .split('\n')
  .filter(Boolean)
  .map(Number);
if (pair.length !== 2 || pair.some((value) => !Number.isSafeInteger(value) || value <= 0)) {
  throw new Error(`game ${game} needs two participations with canonical solves`);
}

const accounts = sql(
  `SELECT id::text || '|' || security_stamp || '|' || role::text ` +
    `FROM "AspNetUsers" WHERE role IN (2,3) AND security_stamp IS NOT NULL ORDER BY id LIMIT 32`,
)
  .split('\n')
  .filter(Boolean);
if (accounts.length < 4) throw new Error('at least four disposable Monitor/Admin accounts are required');
const tokens = accounts.map((entry) => {
  const [id, stamp, role] = entry.split('|');
  return mintJwt(id, stamp, Number(role));
});

const boundedResponseText = async (response, byteLimit) => {
  if (!response.body) return '';
  const reader = response.body.getReader();
  const chunks = [];
  let bytes = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    bytes += value.byteLength;
    if (bytes > byteLimit) {
      await reader.cancel();
      throw new Error(`response exceeded ${byteLimit} bytes`);
    }
    chunks.push(Buffer.from(value));
  }
  return Buffer.concat(chunks, bytes).toString('utf8');
};

const validFlowSummary = (row) =>
  row &&
  typeof row.flowId === 'string' &&
  row.flowId.length <= 76 &&
  row.flowId.length % 2 === 0 &&
  /^[a-f\d]+$/i.test(row.flowId) &&
  Number.isSafeInteger(row.connectionPort) &&
  row.connectionPort > 0 &&
  row.connectionPort <= 65_535 &&
  Number.isSafeInteger(row.firstSeenUtc) &&
  row.firstSeenUtc >= 0 &&
  Number.isSafeInteger(row.lastSeenUtc) &&
  row.lastSeenUtc >= row.firstSeenUtc &&
  typeof row.peerIp === 'string' &&
  row.peerIp.length > 0 &&
  row.peerIp.length <= 64 &&
  Number.isSafeInteger(row.packetsIn) &&
  row.packetsIn >= 0 &&
  Number.isSafeInteger(row.packetsOut) &&
  row.packetsOut >= 0 &&
  Number.isSafeInteger(row.bytesIn) &&
  row.bytesIn >= 0 &&
  Number.isSafeInteger(row.bytesOut) &&
  row.bytesOut >= 0 &&
  Number.isSafeInteger(row.flagHits) &&
  row.flagHits >= 0 &&
  typeof row.payloadTruncated === 'boolean';

const seedFlowFixture = async () => {
  const failures = [];
  for (const candidate of flowCandidates) {
    const path =
      `/api/game/captures/${candidate.challengeId}/${candidate.participationId}/` +
      `${encodeURIComponent(candidate.filename)}/flows?page=1&pageSize=1`;
    try {
      const response = await fetch(new URL(path, target), {
        headers: {
          Authorization: `Bearer ${tokens[0]}`,
          'X-Real-IP': '31.254.254.254',
        },
        redirect: 'error',
        signal: AbortSignal.timeout(FLOW_SEED_TIMEOUT_MS),
      });
      const bodyText = await boundedResponseText(response, FLOW_SEED_MAX_RESPONSE_BYTES);
      const retryAfter = response.headers.get('retry-after');
      if (response.status === 503) {
        if (!/^\d+$/.test(String(retryAfter || ''))) {
          throw new Error('busy seed response omitted Retry-After');
        }
        failures.push(`${candidate.filename}: busy`);
        continue;
      }
      if ([400, 404, 413].includes(response.status)) {
        failures.push(`${candidate.filename}: HTTP ${response.status}`);
        continue;
      }
      if (response.status !== 200) throw new Error(`HTTP ${response.status}`);
      let body;
      try {
        body = JSON.parse(bodyText);
      } catch {
        throw new Error('non-JSON flow page');
      }
      if (
        !body ||
        !Array.isArray(body.items) ||
        body.items.length > 1 ||
        !Number.isSafeInteger(body.totalItems) ||
        body.totalItems < body.items.length ||
        typeof body.snapshotVersion !== 'string' ||
        !/^[a-f\d]{32}$/i.test(body.snapshotVersion)
      ) {
        throw new Error('malformed flow seed page');
      }
      if (body.items.length === 0) {
        failures.push(`${candidate.filename}: no TCP flows`);
        continue;
      }
      const summary = body.items[0];
      if (!validFlowSummary(summary)) throw new Error('malformed flow seed summary');
      return {
        challengeId: candidate.challengeId,
        participationId: candidate.participationId,
        filename: candidate.filename,
        snapshotVersion: body.snapshotVersion,
        flowId: summary.flowId,
        connectionPort: summary.connectionPort,
        peerIp: summary.peerIp,
        firstSeenUtc: summary.firstSeenUtc,
        lastSeenUtc: summary.lastSeenUtc,
        direction: summary.packetsOut > 0 ? 'TeamToContainer' : 'ContainerToTeam',
      };
    } catch (error) {
      failures.push(`${candidate.filename}: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  throw new Error(
    `none of ${flowCandidates.length} bounded indexed PCAP candidates produced a TCP flow: ${failures.join('; ')}`,
  );
};

const flow = await seedFlowFixture();

const fixtureDirectory = mkdtempSync(join(tmpdir(), 'rsctf-monitor-evidence-inventory-'));
const fixtureFile = join(fixtureDirectory, 'fixture.json');
writeFileSync(
  fixtureFile,
  JSON.stringify({
    tokens,
    challengeId: bucket[0],
    participationId: bucket[1],
    eventId,
    pair,
    flow,
  }),
  { mode: 0o600 },
);

console.log(
  `monitor evidence/inventory → ${target.origin} game=${game} rate=${rate}/s ` +
    labels.map((label, index) => `${label}=${counts[index]}`).join(' ') +
    ` filesystemFiles=${filesystemFiles} flowPcap=${flow.filename}`,
);

let status = 1;
let child;
let sampler;
try {
  const beforeIo = databaseIo();
  sampleResources();
  sampler = setInterval(() => {
    try {
      sampleResources();
    } catch (error) {
      samplingErrors.push(error instanceof Error ? error.message : String(error));
    }
  }, 1_000);
  const args = ['run'];
  const summaryPath = String(process.env.SUMMARY_JSON || '').trim();
  if (summaryPath) args.push('--summary-export', resolve(summaryPath));
  args.push(new URL('./k6/monitor-evidence-inventory.js', import.meta.url).pathname);
  child = spawn('k6', args, {
    stdio: 'inherit',
    env: {
      ...process.env,
      TARGET: target.origin,
      GAME: String(game),
      FIXTURE_FILE: fixtureFile,
      RATE: String(rate),
      VUS: String(vus),
      DURATION: duration,
    },
  });
  status = await new Promise((resolveStatus, rejectStatus) => {
    child.once('error', rejectStatus);
    child.once('close', (code) => resolveStatus(code ?? 1));
  });
  clearInterval(sampler);
  sampler = undefined;
  sampleResources();
  if (samplingErrors.length > 0) {
    throw new Error(`resource sampling failed: ${samplingErrors.join('; ')}`);
  }
  const resources = validateResourceBounds(beforeIo, databaseIo());
  await exactHealth('post-load');
  const resourcePath =
    process.env.RESOURCE_JSON ||
    (summaryPath
      ? `${resolve(summaryPath)}.resources.json`
      : `/tmp/monitor-evidence-inventory-resources-${Date.now()}.json`);
  writeFileSync(resourcePath, `${JSON.stringify({ limits, resources, samples: resourceSamples }, null, 2)}\n`, {
    mode: 0o600,
  });
  console.log(`monitor_evidence_inventory_resources=${resourcePath}`);
} finally {
  if (sampler) clearInterval(sampler);
  if (child && child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
  rmSync(fixtureDirectory, { recursive: true, force: true });
}
process.exit(status);
