import http from 'k6/http';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

import { expectedPersonalTokenStatus, validTokenPage } from '../personal-token-admission.js';

const TARGET = __ENV.TARGET;
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ''));
const RATE = Number(__ENV.RATE || 10);
const VUS = Number(__ENV.VUS || 24);
if (!TARGET || !TOKENS.valid || !TOKENS.revoked) throw new Error('managed-token fixture is incomplete');

const unexpected = new Rate('personal_token_unexpected');
const unexpected5xx = new Rate('unexpected_server_5xx');
const healthFailure = new Rate('health_failure');
const latency = new Trend('personal_token_ms', true);
const healthLatency = new Trend('health_ms', true);

export const options = {
  scenarios: {
    bearer: {
      executor: 'constant-arrival-rate', rate: RATE, timeUnit: '1s',
      duration: __ENV.DURATION || '20s', preAllocatedVUs: VUS, maxVUs: VUS * 2,
    },
    health: {
      executor: 'constant-arrival-rate', exec: 'health', rate: 1, timeUnit: '1s',
      duration: __ENV.DURATION || '20s', preAllocatedVUs: 2, maxVUs: 4,
    },
  },
  thresholds: {
    personal_token_unexpected: ['rate==0'], unexpected_server_5xx: ['rate==0'],
    health_failure: ['rate==0'], dropped_iterations: ['count==0'],
    personal_token_ms: ['p(95)<3000'], health_ms: ['p(95)<1000'],
  },
};

const kinds = ['valid', 'revoked', 'random', 'nat', 'multisource'];
const randomToken = (sequence) =>
  `rsctf_pat_v1_${String(sequence.toString(36)).padStart(43, 'x').slice(-43)}`;

function tokenFor(kind, sequence) {
  if (kind === 'valid') return TOKENS.valid;
  if (kind === 'revoked' || kind === 'nat') return TOKENS.revoked;
  return randomToken(sequence + 1);
}

function sourceIp(kind, sequence) {
  if (kind === 'nat') return '38.20.20.20';
  if (kind === 'multisource') {
    return `38.${1 + (sequence % 200)}.${1 + (Math.floor(sequence / 200) % 200)}.${1 + (sequence % 250)}`;
  }
  return `38.2.${1 + (sequence % 200)}.${1 + (sequence % 250)}`;
}

export default function () {
  const sequence = exec.scenario.iterationInTest;
  const kind = kinds[sequence % kinds.length];
  const response = http.get(`${TARGET}/api/tokens?count=1&skip=0`, {
    headers: {
      Authorization: `Bearer ${tokenFor(kind, sequence)}`,
      'X-Real-IP': sourceIp(kind, sequence),
    },
    timeout: '4s', responseType: 'text', tags: { credential: kind },
  });
  latency.add(response.timings.duration);
  const retryAfter = String(response.headers['Retry-After'] || response.headers['retry-after'] || '');
  let bodyValid = true;
  if (response.status === 200) {
    try { bodyValid = validTokenPage(response.json()); } catch (_) { bodyValid = false; }
  }
  unexpected.add(!expectedPersonalTokenStatus(kind, response.status, retryAfter) || !bodyValid);
  unexpected5xx.add(response.status >= 500);
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, { responseType: 'text' });
  healthLatency.add(response.timings.duration);
  healthFailure.add(response.status !== 200 || response.body !== 'ok');
  unexpected5xx.add(false);
}
