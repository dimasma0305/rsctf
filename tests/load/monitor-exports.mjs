// Read-only fixed-rate acceptance for bounded monitor spreadsheet exports.
// Use a quiescent fixture so the pre/post worksheet row-integrity checks remain exact.
import { execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { PG, RSCTF, TARGET, sleep, sql } from './lib.mjs';
import {
  assertExportRowBound,
  classifyExportResponse,
  worksheetRowCount,
} from './monitor-export-model.js';

const target = new URL(TARGET);
const gameId = positiveInteger(process.env.GAME, 'GAME');
const monitorToken = String(process.env.MONITOR_TOKEN || '').trim();
const rate = positiveInteger(process.env.RATE || 2, 'RATE', 10);
const vus = positiveInteger(process.env.VUS || 4, 'VUS', 32);
const duration = String(process.env.DURATION || '30s');
const maxMemoryDelta = positiveInteger(process.env.MAX_MEMORY_DELTA_MIB || 256, 'MAX_MEMORY_DELTA_MIB', 4096);
const maxTaskDelta = positiveInteger(process.env.MAX_TASK_DELTA || 32, 'MAX_TASK_DELTA', 1024);
const localTarget = ['127.0.0.1', 'localhost', '::1'].includes(target.hostname);
const maxWorksheetXmlBytes = 128 * 1024 * 1024;

if (process.env.MONITOR_EXPORT_STRESS_ACK !== '1') {
  throw new Error('MONITOR_EXPORT_STRESS_ACK=1 is required for this expensive read-only test');
}
if (!monitorToken) throw new Error('MONITOR_TOKEN is required and is never printed');
if (!/^([1-9][0-9]*)(s|m)$/.test(duration)) {
  throw new Error('DURATION must use positive k6 seconds or minutes');
}
if (!localTarget && process.env.ALLOW_REMOTE_MONITOR_EXPORT_STRESS !== target.origin) {
  throw new Error(`remote TARGET requires ALLOW_REMOTE_MONITOR_EXPORT_STRESS=${target.origin}`);
}

function positiveInteger(value, label, maximum = Number.MAX_SAFE_INTEGER) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0 || parsed > maximum) {
    throw new Error(`${label} must be an integer from 1 through ${maximum}`);
  }
  return parsed;
}

async function exactHealth() {
  const started = performance.now();
  try {
    const response = await fetch(new URL('/healthz', target), { signal: AbortSignal.timeout(2_000) });
    const body = await response.text();
    return { ok: response.status === 200 && body === 'ok', ms: performance.now() - started };
  } catch (error) {
    return { ok: false, ms: performance.now() - started, error: String(error) };
  }
}

function memoryBytes(value) {
  const match = String(value).trim().match(/^([0-9.]+)([kmgt]?i?b)$/i);
  if (!match) return null;
  const powers = { b: 0, kb: 1, kib: 1, mb: 2, mib: 2, gb: 3, gib: 3, tb: 4, tib: 4 };
  return Number(match[1]) * 1024 ** (powers[match[2].toLowerCase()] ?? 0);
}

async function resourceSample() {
  const health = await exactHealth();
  if (!localTarget) return { at: Date.now(), health };
  try {
    const stats = execFileSync(
      'docker',
      ['stats', '--no-stream', '--format', '{{.MemUsage}}|{{.CPUPerc}}', RSCTF],
      { encoding: 'utf8' },
    ).trim();
    const [usage, cpu] = stats.split('|');
    let tasks = null;
    try {
      const top = execFileSync('docker', ['top', RSCTF, '-eLo', 'tid='], { encoding: 'utf8' });
      tasks = top.split('\n').map((line) => line.trim()).filter((line) => /^\d+$/.test(line)).length;
    } catch {
      // Some minimal hosts do not support ps -L; memory + health remain authoritative.
    }
    return {
      at: Date.now(),
      health,
      memoryBytes: memoryBytes(String(usage).split('/')[0]),
      cpuPercent: Number(String(cpu).replace('%', '')),
      tasks,
    };
  } catch (error) {
    return { at: Date.now(), health, resourceError: String(error) };
  }
}

function expectedRows(kind) {
  const envName = kind === 'scoreboard' ? 'EXPECTED_SCOREBOARD_ROWS' : 'EXPECTED_SUBMISSION_ROWS';
  if (process.env[envName] !== undefined) {
    return assertExportRowBound(kind, Number(process.env[envName]));
  }
  if (!localTarget) throw new Error(`${envName} is required for a remote TARGET`);
  if (PG === 'rsctf-db-1' && process.env.ALLOW_PRODUCTION_MONITOR_EXPORT_DB !== 'I_ACCEPT') {
    throw new Error(
      `${envName} or an explicit disposable PG_CONTAINER is required; refusing the default production database`,
    );
  }
  const query = kind === 'scoreboard'
    ? `SELECT COUNT(*) FROM "Participations" WHERE game_id=${gameId} AND status=1`
    : `SELECT COUNT(*) FROM "Submissions" WHERE game_id=${gameId}`;
  return assertExportRowBound(kind, Number(sql(query)));
}

async function downloadWithAdmission(kind) {
  const suffix = kind === 'scoreboard' ? 'scoreboardsheet' : 'submissionsheet';
  for (let attempt = 0; attempt < 5; attempt += 1) {
    const response = await fetch(new URL(`/api/game/${gameId}/${suffix}`, target), {
      headers: { Authorization: `Bearer ${monitorToken}` },
      signal: AbortSignal.timeout(25_000),
    });
    const classification = classifyExportResponse(
      response.status,
      response.headers.get('content-type'),
      response.headers.get('retry-after'),
    );
    if (!classification.valid) {
      throw new Error(`${kind} sample returned invalid status/headers (${response.status})`);
    }
    if (classification.admitted) return Buffer.from(await response.arrayBuffer());
    await sleep(Number(response.headers.get('retry-after')) * 1_000);
  }
  throw new Error(`${kind} sample remained overloaded after five retry windows`);
}

function verifyWorkbook(kind, bytes, expected, scratch) {
  const path = join(scratch, `${kind}.xlsx`);
  writeFileSync(path, bytes, { mode: 0o600 });
  const worksheet = execFileSync('unzip', ['-p', path, 'xl/worksheets/sheet1.xml'], {
    encoding: 'utf8',
    maxBuffer: maxWorksheetXmlBytes,
  });
  const actual = worksheetRowCount(worksheet) - 1;
  if (actual !== expected) {
    throw new Error(`${kind} worksheet has ${actual} data rows; PostgreSQL snapshot expected ${expected}`);
  }
}

const preflight = await exactHealth();
if (!preflight.ok) throw new Error(`target healthz is not exactly HTTP 200 / ok: ${JSON.stringify(preflight)}`);

const expected = {
  scoreboard: expectedRows('scoreboard'),
  submissions: expectedRows('submissions'),
};
const scratch = mkdtempSync(join(tmpdir(), 'rsctf-monitor-export-'));
const samples = [];
let sampling = false;
let timer;

try {
  verifyWorkbook('scoreboard', await downloadWithAdmission('scoreboard'), expected.scoreboard, scratch);
  verifyWorkbook('submissions', await downloadWithAdmission('submissions'), expected.submissions, scratch);

  samples.push(await resourceSample());
  timer = setInterval(async () => {
    if (sampling) return;
    sampling = true;
    try {
      samples.push(await resourceSample());
    } finally {
      sampling = false;
    }
  }, 1_000);

  const summaryPath = process.env.SUMMARY_JSON || `/tmp/rsctf-monitor-exports-${Date.now()}.json`;
  const k6Path = new URL('./k6/monitor-exports.js', import.meta.url).pathname;
  const child = spawn('k6', ['run', '--summary-export', summaryPath, k6Path], {
    stdio: 'inherit',
    env: {
      ...process.env,
      TARGET: target.origin,
      GAME: String(gameId),
      MONITOR_TOKEN: monitorToken,
      RATE: String(rate),
      VUS: String(vus),
      DURATION: duration,
    },
  });
  const status = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code) => resolve(code ?? 1));
  });
  if (status !== 0) throw new Error(`k6 monitor export load failed with exit status ${status}`);

  clearInterval(timer);
  timer = undefined;
  while (sampling) await sleep(25);
  samples.push(await resourceSample());

  verifyWorkbook('scoreboard', await downloadWithAdmission('scoreboard'), expected.scoreboard, scratch);
  verifyWorkbook('submissions', await downloadWithAdmission('submissions'), expected.submissions, scratch);

  const healthFailures = samples.filter((sample) => !sample.health.ok).length;
  if (healthFailures !== 0) throw new Error(`healthz failed in ${healthFailures} resource samples`);
  const memory = samples.map((sample) => sample.memoryBytes).filter(Number.isFinite);
  const tasks = samples.map((sample) => sample.tasks).filter(Number.isSafeInteger);
  const memoryDelta = memory.length > 1 ? Math.max(...memory) - memory[0] : null;
  const taskDelta = tasks.length > 1 ? Math.max(...tasks) - tasks[0] : null;
  if (memoryDelta !== null && memoryDelta > maxMemoryDelta * 1024 * 1024) {
    throw new Error(`memory grew by ${(memoryDelta / 1024 / 1024).toFixed(1)} MiB (limit ${maxMemoryDelta} MiB)`);
  }
  if (taskDelta !== null && taskDelta > maxTaskDelta) {
    throw new Error(`runtime tasks/threads grew by ${taskDelta} (limit ${maxTaskDelta})`);
  }

  const defaultResourcePath = summaryPath.endsWith('.json')
    ? summaryPath.replace(/\.json$/, '-resources.json')
    : `${summaryPath}-resources.json`;
  const resourcePath = process.env.RESOURCE_JSON || defaultResourcePath;
  writeFileSync(
    resourcePath,
    `${JSON.stringify({ target: target.origin, gameId, rate, vus, duration, expected, memoryDelta, taskDelta, samples }, null, 2)}\n`,
    { mode: 0o600 },
  );
  console.log(`monitor_exports_ok rows=${expected.scoreboard}/${expected.submissions} memoryDelta=${memoryDelta} taskDelta=${taskDelta}`);
  console.log(`summary=${summaryPath}`);
  console.log(`resources=${resourcePath}`);
} finally {
  if (timer) clearInterval(timer);
  rmSync(scratch, { recursive: true, force: true });
}
