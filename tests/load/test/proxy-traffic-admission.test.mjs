import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import {
  durationMilliseconds,
  endpointOriginMatchesTarget,
  validAdmissionRejection,
  validEndpointRows,
  validTrafficClose,
} from '../proxy-traffic-admission.js';

const scenario = readFileSync(
  new URL('../k6/proxy-traffic-admission.js', import.meta.url),
  'utf8',
);
const runner = readFileSync(
  new URL('../proxy-traffic-admission.mjs', import.meta.url),
  'utf8',
);

test('proxy line-rate fixtures are bounded and recognize the stable policy close', () => {
  assert.equal(validEndpointRows([
    { url: 'wss://example.invalid/api/proxy/id?capability=scoped-secret' },
  ]), true);
  assert.equal(validEndpointRows([]), false);
  assert.equal(validEndpointRows([
    { url: 'https://example.invalid/api/proxy/id?capability=scoped-secret' },
  ]), false);
  assert.equal(validEndpointRows([
    { url: 'wss://user@example.invalid/api/proxy/id?capability=scoped-secret' },
  ]), false);
  assert.equal(validEndpointRows([
    { url: 'wss://example.invalid/api/proxy/id' },
  ]), false);
  assert.equal(validEndpointRows([
    { url: 'wss://example.invalid/api/proxy/id', bearerToken: 'x'.repeat(32) },
  ]), true);
  assert.equal(validEndpointRows([
    { url: 'wss://example.invalid/api/proxy/id', sessionCookie: `RSCTF_Token=${'x'.repeat(32)}` },
  ]), true);
  assert.equal(validEndpointRows(Array.from({ length: 513 }, () => ({
    url: 'wss://example.invalid/api/proxy/id?capability=scoped-secret',
  }))), false);
  const duplicate = {
    url: 'wss://example.invalid/api/proxy/id?capability=scoped-secret',
  };
  assert.equal(validEndpointRows([duplicate, duplicate]), false);
  assert.equal(
    validTrafficClose(1008, 'proxy traffic budget exceeded; retry after 2 seconds'),
    true,
  );
  assert.equal(
    validTrafficClose(1011, 'proxy traffic budget exceeded; retry after 2 seconds'),
    false,
  );
  assert.equal(
    validTrafficClose(1008, 'prefix proxy traffic budget exceeded; retry after 2 seconds'),
    false,
  );
  assert.equal(validAdmissionRejection(429, '2'), true);
  assert.equal(validAdmissionRejection(429, ['2']), true);
  assert.equal(validAdmissionRejection(429, '0'), false);
  assert.equal(validAdmissionRejection(429, undefined), false);
  assert.equal(validAdmissionRejection(503, '2'), false);
});

test('proxy duration parser rejects unbounded and compound schedules', () => {
  assert.equal(durationMilliseconds('30s'), 30_000);
  assert.equal(durationMilliseconds('1h'), 3_600_000);
  assert.equal(durationMilliseconds('0s'), null);
  assert.equal(durationMilliseconds('1h30m'), null);
  assert.equal(durationMilliseconds('forever'), null);
});

test('proxy fixture endpoints must resolve to the configured target origin', () => {
  assert.equal(
    endpointOriginMatchesTarget('wss://ctf.example/api/proxy/id', 'https://ctf.example'),
    true,
  );
  assert.equal(
    endpointOriginMatchesTarget('ws://127.0.0.1:8080/api/proxy/id', 'http://127.0.0.1:8080'),
    true,
  );
  assert.equal(
    endpointOriginMatchesTarget('wss://attacker.example/api/proxy/id', 'https://ctf.example'),
    false,
  );
});

test('proxy traffic drill uses fixed arrivals, bounded work, and an independent health lane', () => {
  assert.match(scenario, /executor: 'constant-arrival-rate'/);
  assert.match(scenario, /RATE > 512/);
  assert.match(scenario, /MAX_VUS > 4096/);
  assert.match(scenario, /durationMs > 3_600_000/);
  assert.match(scenario, /FRAME_BYTES > 65_536/);
  assert.match(scenario, /STREAM_MS > 600_000/);
  assert.match(scenario, /socket\.setInterval/);
  assert.match(scenario, /'X-Forwarded-For'/);
  assert.doesNotMatch(scenario, /'X-Real-IP'/);
  assert.match(scenario, /endpoint\.bearerToken/);
  assert.match(scenario, /endpoint\.sessionCookie/);
  assert.match(scenario, /validTrafficClose\(code, reason\)/);
  assert.match(scenario, /validAdmissionRejection/);
  assert.match(scenario, /proxy_admission_rejections/);
  assert.match(scenario, /health_ms: \['p\(95\)<800'\]/);
  assert.match(scenario, /\/healthz/);
  assert.match(scenario, /dropped_iterations: \['count==0'\]/);
  assert.match(runner, /PROXY_TRAFFIC_LOAD_ACK/);
  assert.match(runner, /ALLOW_REMOTE_PROXY_TRAFFIC_LOAD/);
  assert.match(runner, /endpointOriginMatchesTarget/);
  assert.match(runner, /metadata\.mode & 0o077/);
  assert.match(runner, /metadata\.size > 6 \* 1024 \* 1024/);
  assert.match(runner, /fixture changed during the run/);
});
