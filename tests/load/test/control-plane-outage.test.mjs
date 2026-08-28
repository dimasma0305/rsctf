import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { isBoundedImageFailure, validHealthyProxyEndpoints, validWorkerInventory } from '../control-plane-outage.js';

const scenario = readFileSync(new URL('../k6/control-plane-outage.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../control-plane-outage.mjs', import.meta.url), 'utf8');

test('worker and missing-image outage contracts fail closed', () => {
  const row = { id: 'abc', online: false, sessionEpoch: 2, capacity: { slots: 4 } };
  assert.equal(validWorkerInventory([row], 'abc', false), true);
  assert.equal(validWorkerInventory([row], 'abc', true), false);
  assert.equal(isBoundedImageFailure(503, 'worker-local image is not present'), true);
  assert.equal(isBoundedImageFailure(500, 'image'), false);
  assert.equal(validHealthyProxyEndpoints([
    { kind: 'player', workerId: 'other', url: 'ws://local/player', token: 'x'.repeat(16) },
    { kind: 'checker', workerId: 'other', url: 'ws://local/checker', token: 'y'.repeat(16) },
  ], 'outage'), true);
  assert.equal(validHealthyProxyEndpoints([
    { kind: 'player', workerId: 'outage', url: 'ws://local/player', token: 'x'.repeat(16) },
    { kind: 'checker', workerId: 'other', url: 'ws://local/checker', token: 'y'.repeat(16) },
  ], 'outage'), false);
});

test('outage harness is fixed-rate, health-gated, acknowledged, and restorative', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /\/api\/admin\/workers/);
  assert.match(scenario, /\/healthz/);
  assert.match(scenario, /binaryMessage/);
  assert.match(scenario, /PROXY_ENDPOINTS_FILE/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(runner, /CONFIRM_WORKER_OUTAGE/);
  assert.match(runner, /HEALTHY_PROXY_ENDPOINTS_FILE/);
  assert.match(runner, /CONTROL_PLANE_IMAGE_OUTAGE_ACK/);
  assert.match(runner, /CONFIRM_REMOTE_IMAGE_OUTAGE/);
  assert.match(runner, /finally \{/);
  assert.match(runner, /docker.*start|command\(\['start'/s);
  assert.match(runner, /X-RSCTF-Operation-ID/);
  assert.match(runner, /container_id IS NOT NULL/);
});
