import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/honeypot-bounds.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../honeypot-bounds.mjs', import.meta.url), 'utf8');

test('honeypot load uses fixed arrivals and preserves the exact decoy response', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /sequence % 2 === 0 \? 'GET' : 'POST'/);
  assert.match(scenario, /response\.status !== 404 \|\| response\.body !== 'Not Found'/);
  assert.match(scenario, /honeypot_decoy_ms: \['p\(95\)<500'\]/);
  assert.match(scenario, /honeypot_health_ms: \['p\(95\)<500'\]/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(scenario, /'X-Forwarded-For'/);
  assert.doesNotMatch(scenario, /X-Real-IP/);
  assert.doesNotMatch(scenario, /Authorization/);
});

test('runner gates persistent writes, storage, slow sockets, resources, and health', () => {
  assert.match(runner, /HONEYPOT_STRESS_ACK/);
  assert.match(runner, /ALLOW_REMOTE_HONEYPOT_STRESS/);
  assert.match(runner, /aggregateSnapshot/);
  assert.match(runner, /beforeHits/);
  assert.match(runner, /hitDelta/);
  assert.match(runner, /changedSources\.size/);
  assert.match(runner, /target may not trust X-Forwarded-For/);
  assert.match(runner, /source_hash IS NOT NULL/);
  assert.match(runner, /OCTET_LENGTH\(bait\)/);
  assert.match(runner, /OCTET_LENGTH\(user_agent\)/);
  assert.match(runner, /TCP slow-loris socket outlived/);
  assert.match(runner, /connectedSlowSockets < 1/);
  assert.match(runner, /docker', \['stats'/);
  assert.match(runner, /docker', \['top'/);
  assert.match(runner, /body !== 'ok'/);
  assert.match(runner, /newRows > maxRows/);
});
