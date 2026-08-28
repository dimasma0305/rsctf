import http from 'k6/http';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

import { HONEYPOT_BAITS, validDecoyResponse } from '../honeypot-admission.js';

const TARGET = __ENV.TARGET;
const MARKER = __ENV.MARKER;
const RATE = Number(__ENV.RATE || 60);
const VUS = Number(__ENV.VUS || 24);
if (!TARGET || !MARKER) throw new Error('TARGET and MARKER are required');

const decoyFailure = new Rate('honeypot_decoy_failure');
const server5xx = new Rate('server_5xx');
const healthFailure = new Rate('health_failure');
const latency = new Trend('honeypot_ms', true);
const healthLatency = new Trend('health_ms', true);

export const options = {
  scenarios: {
    decoys: {
      executor: 'constant-arrival-rate',
      rate: RATE,
      timeUnit: '1s',
      duration: __ENV.DURATION || '10s',
      preAllocatedVUs: VUS,
      maxVUs: VUS * 2,
    },
    health: {
      executor: 'constant-arrival-rate',
      exec: 'health',
      rate: 1,
      timeUnit: '1s',
      duration: __ENV.DURATION || '10s',
      preAllocatedVUs: 2,
      maxVUs: 4,
    },
  },
  thresholds: {
    honeypot_decoy_failure: ['rate==0'],
    server_5xx: ['rate==0'],
    health_failure: ['rate==0'],
    dropped_iterations: ['count==0'],
    honeypot_ms: ['p(95)<1000'],
    health_ms: ['p(95)<1000'],
  },
};

export default function () {
  const sequence = exec.scenario.iterationInTest;
  const bait = HONEYPOT_BAITS[sequence % HONEYPOT_BAITS.length];
  const source = `38.${1 + (sequence % 200)}.${1 + (Math.floor(sequence / 200) % 200)}.${1 + (sequence % 250)}`;
  const response = http.get(`${TARGET}${bait}`, {
    headers: { 'User-Agent': MARKER, 'X-Real-IP': source },
    responseType: 'text',
    tags: { endpoint: 'honeypot' },
  });
  latency.add(response.timings.duration);
  decoyFailure.add(!validDecoyResponse(response.status, String(response.body || '')));
  server5xx.add(response.status >= 500);
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, { responseType: 'text' });
  healthLatency.add(response.timings.duration);
  healthFailure.add(response.status !== 200 || response.body !== 'ok');
  server5xx.add(response.status >= 500);
}
