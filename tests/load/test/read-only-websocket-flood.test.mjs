import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { signalrHandshake, unsupportedSignalrInvocation, validAbuseClose } from '../read-only-websocket-flood.js';

const scenario = readFileSync(new URL('../k6/read-only-websocket-flood.js', import.meta.url), 'utf8');
const runner = readFileSync(new URL('../read-only-websocket-flood.mjs', import.meta.url), 'utf8');

test('read-only feed protocol fixtures are exact and reject by policy or frame size', () => {
  assert.equal(signalrHandshake(), '{"protocol":"json","version":1}\u001e');
  assert.match(unsupportedSignalrInvocation(7), /"type":1.*"target":"Upload".*\u001e$/);
  assert.equal(validAbuseClose(1008), true);
  assert.equal(validAbuseClose(1009), true);
  assert.equal(validAbuseClose(1000), false);
});

test('websocket flood uses fixed connection arrivals and an independent health lane', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /\/hub\/attack\?game=/);
  assert.match(scenario, /\/hub\/attack\/ws\?game=/);
  assert.match(scenario, /'X-Real-IP': sourceIp/);
  assert.match(scenario, /socket\.send\('x'\.repeat\(FRAME_BYTES\)\)/);
  assert.match(scenario, /\/healthz/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(runner, /READONLY_WS_FLOOD_ACK/);
  assert.match(runner, /ALLOW_REMOTE_READONLY_WS_FLOOD/);
  assert.equal((runner.match(/livePublic\(\) !== '1'/g) || []).length, 2);
});
