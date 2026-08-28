import http from 'k6/http';
import exec from 'k6/execution';
import { Counter, Rate, Trend } from 'k6/metrics';

import { expectedBearerStatus, validTargetModel } from '../ad-bearer-admission.js';

const TARGET = __ENV.TARGET;
const GAME = __ENV.GAME;
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ''));
const MODE = __ENV.MODE || 'mixed';
const RATE = Number(__ENV.RATE || 10);
const VUS = Number(__ENV.VUS || 20);
if (!TARGET || !/^\d+$/.test(GAME) || !TOKENS.valid || !TOKENS.revoked) throw new Error('A&D bearer fixture is incomplete');

const unexpected = new Rate('ad_bearer_unexpected');
const unexpected5xx = new Rate('unexpected_server_5xx');
const healthFailure = new Rate('health_failure');
const latency = new Trend('ad_bearer_ms', true);
const healthLatency = new Trend('health_ms', true);
const slowTimeouts = new Counter('ad_bearer_slow_timeouts');

const randomToken = (sequence) => `ad_${String(sequence.toString(36)).padStart(43, 'x').slice(-43)}`;
const mixedKinds = ['valid', 'revoked', 'random', 'nat', 'multisource', ...(TOKENS.rotated ? ['rotated'] : []), ...(TOKENS.suspended ? ['suspended'] : [])];

const thresholds = {
  ad_bearer_unexpected: ['rate==0'], unexpected_server_5xx: ['rate==0'],
  health_failure: ['rate==0'], dropped_iterations: ['count==0'],
  ad_bearer_ms: ['p(95)<3000'], health_ms: ['p(95)<1000'],
};
if (MODE === 'slow') thresholds.ad_bearer_slow_timeouts = ['count>0'];

export const options = {
  scenarios: {
    bearer: { executor: 'constant-arrival-rate', rate: RATE, timeUnit: '1s', duration: __ENV.DURATION || '20s', preAllocatedVUs: VUS, maxVUs: VUS * 2 },
    health: { executor: 'constant-arrival-rate', exec: 'health', rate: 1, timeUnit: '1s', duration: __ENV.DURATION || '20s', preAllocatedVUs: 2, maxVUs: 4 },
  },
  thresholds,
};

function kindFor(sequence) {
  if (MODE === 'slow') return 'slow';
  if (MODE === 'loop') return 'loop';
  if (MODE === 'redis-loss') return mixedKinds[sequence % mixedKinds.length];
  return mixedKinds[sequence % mixedKinds.length];
}

function tokenFor(kind, sequence) {
  if (kind === 'valid' || kind === 'slow') return TOKENS.valid;
  if (kind === 'revoked' || kind === 'loop') return TOKENS.revoked;
  if (kind === 'rotated') return TOKENS.rotated;
  if (kind === 'suspended') return TOKENS.suspended;
  return randomToken(sequence + 1);
}

function sourceIp(kind, sequence) {
  if (kind === 'nat' || kind === 'loop') return '38.10.10.10';
  if (kind === 'multisource') return `38.${1 + (sequence % 200)}.${1 + (Math.floor(sequence / 200) % 200)}.${1 + (sequence % 250)}`;
  return `38.1.${1 + (sequence % 200)}.${1 + (sequence % 250)}`;
}

export default function () {
  const sequence = exec.scenario.iterationInTest;
  const kind = kindFor(sequence);
  const response = http.get(`${TARGET}/api/Game/${GAME}/Ad/Targets`, {
    headers: { Authorization: `Bearer ${tokenFor(kind, sequence)}`, 'X-Real-IP': sourceIp(kind, sequence) },
    timeout: '4s', responseType: 'text', tags: { credential: kind, outage: MODE },
  });
  latency.add(response.timings.duration);
  const statusValid = expectedBearerStatus(
    kind,
    response.status,
    String(response.headers['Retry-After'] || response.headers['retry-after'] || ''),
  );
  let bodyValid = true;
  if (response.status === 200) {
    try { bodyValid = validTargetModel(response.json()); } catch (_) { bodyValid = false; }
  }
  unexpected.add(!statusValid || !bodyValid);
  if (kind === 'slow' && response.status === 503) slowTimeouts.add(1);
  unexpected5xx.add(response.status >= 500 && !(kind === 'slow' && response.status === 503));
}

export function health() {
  const responses = http.batch([
    ['GET', `${TARGET}/livez`, null, { responseType: 'text' }],
    ['GET', `${TARGET}/healthz`, null, { responseType: 'text' }],
  ]);
  healthLatency.add(Math.max(...responses.map((response) => response.timings.duration)));
  const [live, ready] = responses;
  const readyValid = MODE === 'redis-loss'
    ? ready.status === 503 || (ready.status === 200 && ready.body === 'ok')
    : ready.status === 200 && ready.body === 'ok';
  healthFailure.add(live.status !== 200 || live.body !== 'ok' || !readyValid);
  unexpected5xx.add(false);
}
