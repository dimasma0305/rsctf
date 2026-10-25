// Scoped fixed-rate API comparison. Seed data separately; this runner never mutates it.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { mintJwt, sql, TARGET, PG } from './lib.mjs';

const game = Number(process.env.GAME);
const teams = Number(process.env.TEAMS || 500);
const mode = process.env.SCOREBOARD_CASE || 'page';
const pid = Number(process.env.SCOREBOARD_APP_PID);
const summary = String(process.env.SUMMARY_JSON || '').trim();
if (!Number.isSafeInteger(game) || game < 1 || !Number.isInteger(teams) || teams < 100 || teams > 10_000
  || !Number.isInteger(pid) || pid < 1 || !['full', 'page'].includes(mode) || !summary) {
  throw new Error('GAME, TEAMS=100..10000, SCOREBOARD_APP_PID, SUMMARY_JSON, and SCOREBOARD_CASE=full|page required');
}
const users = sql(`SELECT id::text || '|' || security_stamp FROM "AspNetUsers"
  WHERE user_name LIKE 'PLATFORM_BENCH_%' AND role = 1 AND security_stamp IS NOT NULL ORDER BY id LIMIT 4000`)
  .split('\n').filter(Boolean);
if (users.length < 100) throw new Error('100+ disposable PLATFORM_BENCH_ accounts required');
const tokens = users.map(row => { const [id, stamp] = row.split('|'); return mintJwt(id, stamp, 1); });

async function board(path) {
  const response = await fetch(`${TARGET}${path}`, { signal: AbortSignal.timeout(15_000) });
  assert.equal(response.status, 200, `${path}: ${response.status}`);
  return response.json();
}
async function integrity() {
  const full = await board(`/api/game/${game}/scoreboard`);
  assert.equal(full.items.length, teams);
  assert.equal(new Set(full.items.map(row => row.id)).size, teams);
  const ids = [];
  let version;
  for (let skip = 0; skip < teams; skip += 100) {
    const page = await board(`/api/game/${game}/scoreboard/page?skip=${skip}&count=100`);
    version ??= page.snapshotVersion;
    assert.equal(page.snapshotVersion, version, 'fixture changed during page scan');
    assert.deepEqual(page.items, full.items.slice(skip, skip + 100));
    ids.push(...page.items.map(row => row.id));
  }
  assert.equal(new Set(ids).size, teams);
  const firstPage = await board(`/api/game/${game}/scoreboard/page?skip=0&count=30`);
  assert.deepEqual(firstPage.items, full.items.slice(0, 30));
  const exported = await board(`/api/game/${game}/scoreboard/ctftime`);
  assert.deepEqual(exported.standings, full.items.filter(row => row.rank > 0)
    .map(row => ({ pos: row.rank, team: row.name, score: row.score })));
  assert.equal(sql(`SELECT COUNT(*) FROM (SELECT participation_id, challenge_id FROM "FirstSolves"
    WHERE participation_id IN (SELECT id FROM "Participations" WHERE game_id=${game})
    GROUP BY participation_id, challenge_id HAVING COUNT(*) > 1) duplicate`), '0');
  return { teams, version, firstSolveCount: Number(sql(`SELECT COUNT(*) FROM "FirstSolves"
    WHERE participation_id IN (SELECT id FROM "Participations" WHERE game_id=${game})`)) };
}

const before = await integrity();
const directory = mkdtempSync(join(tmpdir(), 'rsctf-scoreboard-page-'));
const tokenFile = join(directory, 'tokens.json');
writeFileSync(tokenFile, JSON.stringify(tokens), { mode: 0o600 });
const samples = [];
const errors = [];
const ticksPerSecond = Number(execFileSync('getconf', ['CLK_TCK'], { encoding: 'utf8' }).trim());
let previous;
const sample = async () => {
  const at = performance.now();
  const fields = readFileSync(`/proc/${pid}/stat`, 'utf8').split(') ')[1].split(' ');
  const ticks = Number(fields[11]) + Number(fields[12]);
  const memory = Number(readFileSync(`/proc/${pid}/status`, 'utf8').match(/^VmRSS:\s+(\d+)/m)?.[1]) * 1024;
  const pg = JSON.parse(execFileSync('docker', ['stats', '--no-stream', '--format', '{{json .}}', PG], { encoding: 'utf8' }));
  const started = performance.now();
  const health = await fetch(`${TARGET}/healthz`, { signal: AbortSignal.timeout(2_000) });
  if (health.status !== 200 || await health.text() !== 'ok') throw new Error('healthz must remain exactly ok');
  if (previous) samples.push({ at, appCpuPercent: (ticks - previous.ticks) / ticksPerSecond / ((at - previous.at) / 1000) * 100,
    appRssBytes: memory, postgresCpuPercent: Number(pg.CPUPerc.replace('%', '')), postgresMemory: pg.MemUsage,
    healthMilliseconds: performance.now() - started });
  previous = { at, ticks };
};
let stopped = false;
const sampler = (async () => {
  while (!stopped) {
    try { await sample(); } catch (error) { errors.push(error.message); }
    if (!stopped) await new Promise(resolve => setTimeout(resolve, 1000));
  }
})();
let exitCode;
try {
  const child = spawn('k6', ['run', '--summary-export', resolve(summary),
    new URL('./k6/scoreboard-page.js', import.meta.url).pathname], {
    stdio: 'inherit', env: { ...process.env, TARGET, TOKENS_FILE: tokenFile, SCOREBOARD_CASE: mode },
  });
  exitCode = await new Promise((resolveChild, reject) => {
    child.once('error', reject); child.once('exit', code => resolveChild(code ?? 1));
  });
} finally {
  stopped = true;
  await sampler;
  rmSync(directory, { recursive: true, force: true });
}
const after = await integrity();
assert.deepEqual(after, before);
writeFileSync(`${resolve(summary)}.resources.json`, JSON.stringify({ before, after, samples, errors }, null, 2));
assert.equal(errors.length, 0, errors.join('\n'));
assert.ok(samples.length >= 3, 'at least three resource samples required');
assert.equal(exitCode, 0, 'k6 thresholds failed');
console.log(JSON.stringify({ mode, conditional: process.env.CONDITIONAL === '1', integrity: after, resourceSamples: samples.length }));
