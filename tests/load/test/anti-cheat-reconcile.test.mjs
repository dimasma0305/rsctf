import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const scenario = readFileSync(new URL('../k6/anti-cheat-reconcile.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../anti-cheat-reconcile.mjs', import.meta.url), 'utf8');

test('anti-cheat reconciliation load is fixed-rate and keeps health in-band', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /server_5xx: \['rate==0'\]/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(scenario, /response\.body !== 'ok'/);
});

test('large-ledger gate requires explicit mutation acknowledgement and proves idle cursors', () => {
  assert.match(runner, /ANTI_CHEAT_RECONCILE_STRESS_ACK !== '1'/);
  assert.match(runner, /MIN_SOURCE_ROWS \|\| 10_000/);
  assert.match(runner, /fields\[5\] <= fields\[6\] && fields\[7\] === 0/);
  assert.match(runner, /idle reconciliation left source cursors behind/);
  assert.match(runner, /source ledger changed/);
});
