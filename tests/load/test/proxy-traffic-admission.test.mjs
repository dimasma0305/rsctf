import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { validEndpointRows, validTrafficClose } from '../proxy-traffic-admission.js';

const scenario = readFileSync(new URL('../k6/proxy-traffic-admission.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../proxy-traffic-admission.mjs', import.meta.url), 'utf8');

test('proxy line-rate fixtures are bounded and recognize the stable policy close', () => {
  assert.equal(validEndpointRows([{ url: 'wss://example.invalid/api/proxy/id', token: 'x'.repeat(32) }]), true);
  assert.equal(validEndpointRows([]), false);
  assert.equal(validTrafficClose(1008, 'proxy traffic budget exceeded; retry after 2 seconds'), true);
  assert.equal(validTrafficClose(1011, 'proxy traffic budget exceeded; retry after 2 seconds'), false);
});

test('proxy traffic drill uses fixed arrivals, bounded frames, and an independent health lane', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /FRAME_BYTES > 65_536/);
  assert.match(scenario, /socket\.setInterval/);
  assert.match(scenario, /validTrafficClose\(code, reason\)/);
  assert.match(scenario, /\/healthz/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(runner, /PROXY_TRAFFIC_LOAD_ACK/);
  assert.match(runner, /ALLOW_REMOTE_PROXY_TRAFFIC_LOAD/);
});
