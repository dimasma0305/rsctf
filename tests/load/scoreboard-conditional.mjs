// Mint a disposable spectator cohort, run the conditional scoreboard scenario,
// and sample the exact app/PostgreSQL containers at a fixed interval.
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { mintJwt, sql, TARGET } from './lib.mjs';

const STANDARD_GAME = Number(process.env.STANDARD_GAME || process.env.JEO_GAME);
const KOTH_GAME = Number(process.env.KOTH_GAME || process.env.AD_GAME || process.env.GAME);
const TOKEN_COUNT = Number(process.env.TOKEN_COUNT || 500);
const RATE = Number(process.env.RATE || 200);
const VUS = Number(process.env.VUS || 100);
const DURATION = String(process.env.DURATION || '60s');
const durationMatch = DURATION.match(/^([1-9]\d*)(s|m)$/);
const durationSeconds = durationMatch
  ? Number(durationMatch[1]) * (durationMatch[2] === 'm' ? 60 : 1)
  : 0;
const SUMMARY_JSON = String(process.env.SUMMARY_JSON || '').trim();
const resourceContainers = String(
  process.env.SCOREBOARD_RESOURCE_CONTAINERS || 'rsctf-rsctf-1,rsctf-rsctf-2,rsctf-db-1',
)
  .split(',')
  .map((value) => value.trim())
  .filter(Boolean);

if (
  resourceContainers.length < 1 ||
  resourceContainers.length > 16 ||
  !resourceContainers.every((name) => /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(name))
) {
  throw new Error('SCOREBOARD_RESOURCE_CONTAINERS requires 1..16 bounded Docker container names');
}

if (![STANDARD_GAME, KOTH_GAME].every((id) => Number.isSafeInteger(id) && id > 0)) {
  throw new Error('positive STANDARD_GAME and KOTH_GAME are required');
}
if (!Number.isSafeInteger(TOKEN_COUNT) || TOKEN_COUNT < 100 || TOKEN_COUNT > 4000) {
  throw new Error('TOKEN_COUNT must be between 100 and 4000');
}
if (
  !Number.isSafeInteger(RATE) || RATE <= 0 || RATE > 2000 ||
  !Number.isSafeInteger(VUS) || VUS <= 0 || VUS > 500 ||
  !Number.isSafeInteger(durationSeconds) || durationSeconds <= 0 || durationSeconds > 600
) {
  throw new Error('RATE must be 1..2000, VUS 1..500, and DURATION 1s..10m');
}

const accounts = sql(
  `SELECT id::text || '|' || security_stamp
     FROM "AspNetUsers"
    WHERE security_stamp IS NOT NULL
      AND (user_name LIKE 'LT_%' OR user_name LIKE 'LOADTEST%' OR email LIKE '%@load.test')
    ORDER BY id
    LIMIT ${TOKEN_COUNT}`,
)
  .split('\n')
  .filter(Boolean);
if (accounts.length < 100) {
  throw new Error(`at least 100 disposable load-test accounts are required; found ${accounts.length}`);
}
const tokens = accounts.map((entry) => {
  const [id, stamp] = entry.split('|');
  return mintJwt(id, stamp, 1);
});

const parseBytes = (value) => {
  const match = String(value).trim().match(/^([0-9.]+)([kmgt]?i?b)$/i);
  if (!match) return 0;
  const power = { b: 0, kb: 1, kib: 1, mb: 2, mib: 2, gb: 3, gib: 3 }[match[2].toLowerCase()];
  return Number(match[1]) * 1024 ** (power ?? 0);
};

const samples = [];
const samplingErrors = [];
const sampleResources = () => {
  const sampled = spawnSync(
    'docker',
    ['stats', '--no-stream', '--format', '{{json .}}', ...resourceContainers],
    { encoding: 'utf8' },
  );
  if (sampled.status !== 0) {
    throw new Error(`docker stats failed: ${(sampled.stderr || sampled.stdout || '').trim()}`);
  }
  for (const line of sampled.stdout.split('\n').filter(Boolean)) {
    const row = JSON.parse(line);
    const cpuPercent = Number(String(row.CPUPerc || '').replace('%', ''));
    const memoryBytes = parseBytes(String(row.MemUsage || '').split('/')[0]);
    if (!resourceContainers.includes(row.Name) || !Number.isFinite(cpuPercent) || memoryBytes <= 0) {
      throw new Error(`invalid docker stats sample for ${String(row.Name || 'unknown')}`);
    }
    samples.push({
      at: Date.now(),
      name: row.Name,
      cpuPercent,
      memoryBytes,
    });
  }
};

const summarize = (values) => {
  const sorted = values.filter(Number.isFinite).sort((left, right) => left - right);
  const percentile = (fraction) => sorted[Math.max(0, Math.ceil(sorted.length * fraction) - 1)] || 0;
  return {
    samples: sorted.length,
    average: sorted.reduce((sum, value) => sum + value, 0) / Math.max(1, sorted.length),
    p95: percentile(0.95),
    max: sorted.at(-1) || 0,
  };
};

const tokenDirectory = mkdtempSync(join(tmpdir(), 'rsctf-scoreboard-conditional-'));
const tokenFile = join(tokenDirectory, 'tokens.json');
writeFileSync(tokenFile, JSON.stringify(tokens), { mode: 0o600 });
const k6Args = ['run'];
if (SUMMARY_JSON) k6Args.push('--summary-export', resolve(SUMMARY_JSON));
k6Args.push(new URL('./k6/scoreboard-conditional.js', import.meta.url).pathname);
const child = spawn('k6', k6Args, {
  stdio: 'inherit',
  env: {
    ...process.env,
    TARGET,
    STANDARD_GAME: String(STANDARD_GAME),
    KOTH_GAME: String(KOTH_GAME),
    TOKENS_FILE: tokenFile,
    RATE: String(RATE),
    VUS: String(VUS),
    DURATION,
  },
});

let status = 1;
const sampler = setInterval(() => {
  try {
    sampleResources();
  } catch (error) {
    samplingErrors.push(error instanceof Error ? error.message : String(error));
  }
}, 1_000);
try {
  sampleResources();
  status = await new Promise((resolveExit, rejectExit) => {
    child.once('error', rejectExit);
    child.once('close', (code) => resolveExit(code ?? 1));
  });
  sampleResources();
  if (samplingErrors.length > 0) {
    throw new Error(`resource sampling failed: ${samplingErrors.join('; ')}`);
  }
  const resources = resourceContainers.map((name) => {
    const rows = samples.filter((sample) => sample.name === name);
    if (rows.length === 0) throw new Error(`docker stats returned no samples for ${name}`);
    return {
      name,
      cpuPercent: summarize(rows.map((row) => row.cpuPercent)),
      memoryMiB: {
        ...summarize(rows.map((row) => row.memoryBytes / 1024 / 1024)),
      },
    };
  });
  console.log(JSON.stringify({ resourceSamples: samples.length, resources }, null, 2));
  if (SUMMARY_JSON) {
    writeFileSync(`${resolve(SUMMARY_JSON)}.resources.json`, `${JSON.stringify(resources, null, 2)}\n`, {
      mode: 0o600,
    });
  }
} finally {
  clearInterval(sampler);
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
  rmSync(tokenDirectory, { recursive: true, force: true });
}
process.exit(status);
