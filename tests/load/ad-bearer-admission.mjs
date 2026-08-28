// Fixed-rate admission gate for supported A&D bearer routes. Optional outage
// phases require exact operator acknowledgement and always restore in finally.
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { requireAdToken } from './ad-bearer-admission.js';
import { tokenHash } from './ad-bearer-fixture.mjs';
import { LOAD_DATABASE_URL, PG, PG_DATABASE, PG_USER, runK6, sleep, sql, TARGET } from './lib.mjs';

const game = Number(process.env.AD_BEARER_GAME || process.env.GAME);
if (!Number.isSafeInteger(game) || game <= 0) throw new Error('AD_BEARER_GAME (or GAME) is required');
const targetUrl = new URL(TARGET);
if (process.env.AD_BEARER_STRESS_ACK !== '1') throw new Error('set AD_BEARER_STRESS_ACK=1 for the authentication stress gate');
if (!['127.0.0.1', 'localhost', '::1'].includes(targetUrl.hostname) && process.env.ALLOW_REMOTE_AD_BEARER_STRESS !== targetUrl.origin) {
  throw new Error(`remote target requires ALLOW_REMOTE_AD_BEARER_STRESS=${targetUrl.origin}`);
}
const valid = requireAdToken(process.env.VALID_AD_TOKEN, 'VALID_AD_TOKEN');
const revoked = requireAdToken(process.env.REVOKED_AD_TOKEN, 'REVOKED_AD_TOKEN');
if (valid === revoked) throw new Error('VALID_AD_TOKEN and REVOKED_AD_TOKEN must differ');
const rotated = process.env.ROTATED_AD_TOKEN ? requireAdToken(process.env.ROTATED_AD_TOKEN, 'ROTATED_AD_TOKEN') : '';
const suspended = process.env.SUSPENDED_AD_TOKEN ? requireAdToken(process.env.SUSPENDED_AD_TOKEN, 'SUSPENDED_AD_TOKEN') : '';
const live = sql(`SELECT COUNT(*) FROM "Games" WHERE id=${game} AND start_time_utc<=now() AND now()<=end_time_utc`);
if (live !== '1') throw new Error(`A&D game ${game} must be live`);

function hashCount(token) {
  return Number(sql(`SELECT COUNT(*) FROM "AdTeamApiTokens" WHERE token_hash=decode('${tokenHash(token)}','hex')`));
}
if (hashCount(valid) !== 1) throw new Error('VALID_AD_TOKEN is not current in this database');
if (hashCount(revoked) !== 0) throw new Error('REVOKED_AD_TOKEN is still current in this database');
if (rotated && hashCount(rotated) !== 1) throw new Error('ROTATED_AD_TOKEN is not current in this database');
if (suspended && hashCount(suspended) !== 1) throw new Error('SUSPENDED_AD_TOKEN is not retained for a suspended roster');

function fingerprint() {
  return sql(
    `SELECT COALESCE(string_agg(id::text || ':' || participation_id::text || ':' || encode(token_hash,'hex'),',' ORDER BY id),'') ` +
      `FROM "AdTeamApiTokens"`,
  );
}
const before = fingerprint();
const directory = mkdtempSync(join(tmpdir(), 'rsctf-ad-bearer-'));
const fixtureFile = join(directory, 'tokens.json');
writeFileSync(fixtureFile, JSON.stringify({ valid, revoked, rotated, suspended }), { mode: 0o600 });

function run(mode, suffix = mode) {
  const summary = String(process.env.SUMMARY_JSON || '');
  const summaryPath = summary && suffix ? summary.replace(/(\.json)?$/, `.${suffix}$1`) : summary;
  const status = runK6('ad-bearer-admission.js', {
    TARGET, GAME: game, TOKENS_FILE: fixtureFile, MODE: mode,
    RATE: process.env.RATE || (mode === 'slow' ? 1 : 10),
    VUS: process.env.VUS || (mode === 'slow' ? 8 : 20),
    DURATION: process.env.DURATION || '20s', SUMMARY_JSON: summaryPath,
  });
  if (status !== 0) throw new Error(`A&D bearer ${mode} phase failed with exit ${status}`);
}

function command(program, args) {
  const result = spawnSync(program, args, { encoding: 'utf8' });
  if (result.status !== 0) throw new Error(`${program} ${args.join(' ')} failed: ${(result.stderr || result.stdout).trim()}`);
  return result.stdout.trim();
}

async function waitFor(check, label, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await check()) return;
    await sleep(250);
  }
  throw new Error(`${label} did not settle`);
}

async function redisOutage() {
  const redis = String(process.env.AD_REDIS_CONTAINER || '');
  if (!redis) return;
  if (process.env.CONFIRM_AD_REDIS_OUTAGE !== redis) throw new Error('repeat AD_REDIS_CONTAINER in CONFIRM_AD_REDIS_OUTAGE');
  const origin = new URL(TARGET).origin;
  if (!['127.0.0.1', 'localhost', '::1'].includes(new URL(TARGET).hostname) && process.env.CONFIRM_REMOTE_AD_REDIS_OUTAGE !== origin) {
    throw new Error(`remote Redis outage requires CONFIRM_REMOTE_AD_REDIS_OUTAGE=${origin}`);
  }
  const identity = command('docker', ['inspect', '--format', '{{index .Config.Labels "com.docker.compose.service"}}|{{.State.Running}}', redis]);
  if (identity !== 'redis|true') throw new Error(`${redis} is not a running Compose Redis service`);
  let stopped = false;
  try {
    command('docker', ['stop', '--time', '5', redis]);
    stopped = true;
    await waitFor(async () => (await fetch(new URL('/livez', TARGET))).status === 200, 'Redis outage livez');
    run('redis-loss');
  } finally {
    if (stopped) {
      command('docker', ['start', redis]);
      await waitFor(async () => {
        const response = await fetch(new URL('/healthz', TARGET));
        return response.status === 200 && (await response.text()) === 'ok';
      }, 'Redis recovery');
    }
  }
}

function psqlCommand(query) {
  return LOAD_DATABASE_URL
    ? ['psql', [LOAD_DATABASE_URL, '-X', '-v', 'ON_ERROR_STOP=1', '-qAtc', query]]
    : ['docker', ['exec', PG, 'psql', '-U', PG_USER, '-d', PG_DATABASE, '-X', '-v', 'ON_ERROR_STOP=1', '-qAtc', query]];
}

async function slowPool() {
  if (process.env.AD_SLOW_POOL_STRESS !== '1') return;
  const databaseName = LOAD_DATABASE_URL
    ? new URL(LOAD_DATABASE_URL).pathname.split('/').filter(Boolean).at(-1)
    : PG_DATABASE;
  if (!databaseName || !/(?:test|load|acceptance)/i.test(databaseName)) {
    throw new Error('the slow-pool drill requires a database name containing test, load, or acceptance');
  }
  if (process.env.CONFIRM_AD_SLOW_POOL !== databaseName) {
    throw new Error('repeat the disposable database name in CONFIRM_AD_SLOW_POOL before holding the token table lock');
  }
  const [program, args] = psqlCommand(`BEGIN; LOCK TABLE "AdTeamApiTokens" IN ACCESS EXCLUSIVE MODE; SELECT pg_sleep(60); ROLLBACK;`);
  const child = spawn(program, args, { stdio: 'ignore' });
  try {
    await waitFor(() => sql(
      `SELECT EXISTS(SELECT 1 FROM pg_locks lock JOIN pg_class relation ON relation.oid=lock.relation ` +
        `WHERE relation.relname='AdTeamApiTokens' AND lock.mode='AccessExclusiveLock' AND lock.granted)`,
    ) === 't', 'A&D token table lock');
    run('slow');
  } finally {
    child.kill('SIGTERM');
    if (child.exitCode === null) await new Promise((resolve) => child.once('exit', resolve));
  }
}

try {
  run('mixed', 'baseline');
  run('loop', 'loop');
  await slowPool();
  await redisOutage();
} finally {
  rmSync(directory, { recursive: true, force: true });
}
const after = fingerprint();
if (after !== before) throw new Error('A&D bearer load changed credential ownership');
console.log(`ad_bearer_admission_ok game=${game} redis=${Boolean(process.env.AD_REDIS_CONTAINER)} slow=${process.env.AD_SLOW_POOL_STRESS === '1'}`);
