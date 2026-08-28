import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/details-read.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../details-read.mjs', import.meta.url), 'utf8');

test('player details load uses bounded fixed arrivals and conditional live reads', () => {
  assert.match(scenario, /executor:\s*"constant-arrival-rate"/);
  assert.equal((scenario.match(/http\.get\(/g) || []).length, 2);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  assert.match(scenario, /catalogModel === null/);
  assert.match(scenario, /\/details\/catalog/);
  assert.match(scenario, /\/details\/live/);
  assert.doesNotMatch(scenario, /\/details`/);
  assert.match(scenario, /"If-None-Match"/);
  assert.match(scenario, /response\.status === 304/);
  assert.match(scenario, /details_non_200:\s*\["rate==0"\]/);
  assert.match(scenario, /details_server_5xx:\s*\["rate==0"\]/);
  assert.match(scenario, /dropped_iterations:\s*\["count==0"\]/);
  assert.match(scenario, /RATE > 2000/);
  assert.match(scenario, /VUS > 500/);
  assert.match(scenario, /TOKENS\.length > 4000/);
  assert.match(scenario, /maxVUs:\s*Math\.min\(500, VUS \* 2\)/);
  assert.match(scenario, /durationSeconds > 600/);
});

test('player details runner health-gates the read and bounds disposable credentials', () => {
  assert.match(runner, /assertHealth\("pre-load"\)/);
  assert.match(runner, /assertHealth\("post-load"\)/);
  assert.match(runner, /discover\(\)\.tokens\.slice\(0, 4000\)/);
  assert.match(runner, /writeFileSync\(tokenFile, JSON\.stringify\(tokens\), \{ mode: 0o600 \}\)/);
  assert.match(runner, /rmSync\(tokenDirectory, \{ recursive: true, force: true \}\)/);
});
