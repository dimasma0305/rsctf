// Bounded, read-only live arena acceptance. Never provisions or mutates event data.
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { writeFileSync } from 'node:fs';
const target = process.env.TARGET || 'https://tcp.1pc.tf';
const game = process.env.GAME;
const summary = resolve(process.env.SUMMARY_JSON || '/tmp/rsctf-arena-read.json');
assert.match(target, /^https?:\/\/(127\.0\.0\.1(?::\d+)?|tcp\.1pc\.tf)$/);
assert.match(game || '', /^[1-9]\d*$/);
const containers = (process.env.ARENA_RESOURCE_CONTAINERS || 'rsctf-rsctf-1,rsctf-rsctf-2,rsctf-rsctf-control-1,rsctf-rsctf-proxy-firewall-1,rsctf-db-1').split(',');
assert.ok(containers.length <= 16 && containers.every(c => /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/.test(c)));
const snapshot = async () => {
  const health = await fetch(`${target}/healthz`, {signal:AbortSignal.timeout(5000)});
  assert.equal(health.status,200); assert.equal(await health.text(),'ok');
  const rows = [];
  for (const path of ['scoreboard','ad/scoreboard','ad/koth/scoreboard']) {
    const response = await fetch(`${target}/api/game/${game}/${path}`, {signal:AbortSignal.timeout(5000)});
    assert.equal(response.status,200);
    const body = await response.json(), teams = body.items || body.teams;
    assert.ok(Array.isArray(teams));
    const ids = teams.map(t => String(t.id ?? t.teamId)).sort();
    assert.equal(new Set(ids).size,ids.length,'public roster has no duplicate identities');
    rows.push({path,ids});
  }
  return rows;
};
const samples = [];
const sample = () => samples.push({at:Date.now(),containers:execFileSync('docker',['stats','--no-stream','--format','{{json .}}',...containers],{encoding:'utf8',timeout:10000}).trim().split('\n').map(s=>JSON.parse(s))});
const before = await snapshot();
sample();
const child = spawn('k6',['run','--summary-export',summary,new URL('./k6/arena-read.js',import.meta.url).pathname],{stdio:'inherit',env:{...process.env,TARGET:target,GAME:game}});
const samplingErrors = [];
const timer = setInterval(()=>{try{sample()}catch(error){samplingErrors.push(String(error))}},3000);
let code;
try {
  code = await new Promise((resolve,reject)=>{child.once('error',reject);child.once('close',resolve)});
} finally {
  clearInterval(timer);
  if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
}
sample();
const after = await snapshot();
writeFileSync(`${summary}.resources.json`,JSON.stringify({target,game,before,after,samples,samplingErrors},null,2));
assert.equal(code,0); assert.deepEqual(samplingErrors,[]); assert.deepEqual(after,before,'read-only load preserves the roster');
