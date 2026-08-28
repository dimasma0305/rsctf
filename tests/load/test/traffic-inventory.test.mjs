import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { captureFingerprint, validTrafficRows } from '../traffic-inventory.js';

const scenario = readFileSync(new URL('../k6/traffic-inventory.js', import.meta.url), 'utf8');

test('traffic response contracts bound and deduplicate every page kind', () => {
  assert.equal(validTrafficRows([{ id: 1, title: 'x', count: 0 }], 'games', 500, 12), true);
  assert.equal(validTrafficRows([{ id: 1, teamId: 2, name: 'x', count: 3 }], 'teams', 100, 12), true);
  assert.equal(validTrafficRows([{ fileName: 'x.pcap', size: 3, updateTime: 4 }], 'files', 100, 12), true);
  assert.equal(validTrafficRows([{ id: 1, teamId: 2, name: 'x', count: 3 }, { id: 1, teamId: 2, name: 'x', count: 3 }], 'teams'), false);
  assert.equal(captureFingerprint([{ id: 2, count: 1 }, { id: 1, count: 3 }]), '1:3|2:1');
});

test('traffic inventory scenario is fixed-rate, paged, read-only, and health-gated', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /count=100&skip=100/);
  assert.doesNotMatch(scenario, /http\.(?:post|put|patch|del|delete)\(/);
  assert.match(scenario, /\/healthz/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
});
