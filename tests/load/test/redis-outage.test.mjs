import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const runner = readFileSync(new URL('../redis-outage.mjs', import.meta.url), 'utf8');
const scenario = readFileSync(new URL('../k6/redis-outage.js', import.meta.url), 'utf8');

test('Redis outage runner requires an exact disposable-container acknowledgement', () => {
  assert.match(runner, /ACK !== REDIS/);
  assert.match(runner, /com\.docker\.compose\.service/);
  assert.match(runner, /identity !== 'redis\|true'/);
  assert.match(runner, /finally \{/);
  assert.match(runner, /command\('docker', \['start', REDIS\]\)/);
});

test('Redis outage load is fixed-rate and treats only the expected 400 as success', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /expectedStatuses\(400\)/);
  assert.match(scenario, /http_req_duration: \['p\(95\)<1000'\]/);
  assert.match(scenario, /unexpected_status: \['rate==0'\]/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
});
