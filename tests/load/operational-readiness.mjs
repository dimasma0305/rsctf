// Fixed-rate read-only regression for frozen exclusions and blocked imports.
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { GAME, PG, TARGET, mintJwt, sql } from './lib.mjs';

const game = Number(GAME), binding = Number(process.env.BINDING);
assert.ok(Number.isSafeInteger(game) && game > 0 && Number.isSafeInteger(binding) && binding > 0);
assert.ok(['127.0.0.1', 'localhost'].includes(new URL(TARGET).hostname), 'use a disposable local stack');
assert.match(sql(`SELECT title FROM "Games" WHERE id=${game}`), /^READINESS-LOAD-/);
assert.equal(sql(`SELECT status FROM "RepoBindings" WHERE id=${binding}`), '1', 'fixture binding must be paused');
const [admin, stamp] = sql(`SELECT id::text || '|' || security_stamp FROM "AspNetUsers" WHERE role=3 ORDER BY id LIMIT 1`).split('|');
assert.ok(admin && stamp);
const pid = Number(process.env.READINESS_SERVER_PID);
assert.ok(Number.isSafeInteger(pid) && pid > 0);
assert.match(execFileSync('ps', ['-p', String(pid), '-o', 'comm='], { encoding: 'utf8' }), /rsctf/);
assert.match(PG, /^[A-Za-z0-9_.-]+$/);

function integrity() {
  return sql(`SELECT jsonb_build_array(
    (SELECT md5(COALESCE(jsonb_agg(to_jsonb(c))::text,'[]')) FROM "KothOfficialConfigs" c WHERE game_id=${game}),
    (SELECT md5(COALESCE(jsonb_agg(to_jsonb(r) ORDER BY id)::text,'[]')) FROM "AdRounds" r WHERE game_id=${game}),
    (SELECT md5(COALESCE(jsonb_agg(to_jsonb(r) ORDER BY id)::text,'[]')) FROM "KothControlResults" r WHERE game_id=${game}),
    (SELECT jsonb_build_array(scan_failures,scan_outcome,last_commit_sha,blocked_commit_sha) FROM "RepoBindings" WHERE id=${binding}),
    (SELECT COUNT(*) FROM (SELECT game_id FROM "KothOfficialConfigs" WHERE game_id=${game} GROUP BY game_id HAVING COUNT(*)>1) duplicates))`);
}
function sample() {
  return {
    at: Date.now(),
    server: execFileSync('ps', ['-p', String(pid), '-o', '%cpu=,rss='], { encoding: 'utf8' }).trim(),
    postgres: JSON.parse(execFileSync('docker', ['stats', '--no-stream', '--format', '{{json .}}', PG], { encoding: 'utf8' })),
  };
}
const before = integrity();
assert.equal(JSON.parse(before)[4], 0);
const samples = [sample()];
const summary = process.env.SUMMARY_JSON || '/tmp/rsctf-readiness-load.json';
const child = spawn('k6', ['run', '--summary-export', summary, new URL('./k6/operational-readiness.js', import.meta.url).pathname], {
  stdio: 'inherit',
  env: { ...process.env, TARGET, GAME: String(game), BINDING: String(binding), ADMIN_TOKEN: mintJwt(admin, stamp) },
});
const samplingErrors = [];
const timer = setInterval(() => { try { samples.push(sample()); } catch (error) { samplingErrors.push(String(error)); } }, 5000);
let status;
try {
  status = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); });
} finally {
  clearInterval(timer);
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
}
samples.push(sample());
const after = integrity();
writeFileSync(`${summary}.integrity.json`, JSON.stringify({ before, after, samples, samplingErrors }, null, 2));
assert.deepEqual(samplingErrors, []);
assert.equal(after, before, 'polling must preserve scoring evidence, exclusions and scan counters');
assert.equal(status, 0);
