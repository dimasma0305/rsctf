import http from 'k6/http';
import exec from 'k6/execution';
import { Rate, Trend } from 'k6/metrics';

import { validConditionalReport, validIncidentPage } from '../anti-cheat-read.js';

const TARGET = __ENV.TARGET;
const GAME = __ENV.GAME;
const TOKENS = JSON.parse(open(__ENV.TOKENS_FILE || ''));
const RATE = Number(__ENV.RATE || 2);
const VUS = Number(__ENV.VUS || 8);
const AFTER = Number(__ENV.DELTA_AFTER || 0);
const ETAG = __ENV.REPORT_ETAG || '';
if (!TARGET || !/^\d+$/.test(GAME) || !TOKENS.length || !Number.isSafeInteger(AFTER) || AFTER < 0 || !ETAG) {
  throw new Error('TARGET, GAME, monitor tokens, DELTA_AFTER, and REPORT_ETAG are required');
}

const invalid = new Rate('anti_cheat_read_invalid');
const server5xx = new Rate('server_5xx');
const rateLimited = new Rate('rate_limited');
const latency = new Trend('anti_cheat_read_ms', true);
const operations = [
  { kind: 'snapshot', path: `/api/game/${GAME}/cheatinfo/page?count=100`, after: 0, ascending: false },
  { kind: 'delta', path: `/api/game/${GAME}/cheatinfo/page?after=${AFTER}&count=100`, after: AFTER, ascending: true },
  { kind: 'report', path: `/api/game/${GAME}/cheatreport` },
];

export const options = {
  scenarios: {
    antiCheatReads: {
      executor: 'constant-arrival-rate', rate: RATE, timeUnit: '1s',
      duration: __ENV.DURATION || '30s', preAllocatedVUs: VUS, maxVUs: VUS * 2,
    },
    health: {
      executor: 'constant-arrival-rate', exec: 'health', rate: 1, timeUnit: '1s',
      duration: __ENV.DURATION || '30s', preAllocatedVUs: 2, maxVUs: 4,
    },
  },
  thresholds: {
    anti_cheat_read_invalid: ['rate==0'], server_5xx: ['rate==0'], rate_limited: ['rate==0'],
    dropped_iterations: ['count==0'], anti_cheat_read_ms: ['p(95)<1000'],
  },
};

export default function () {
  const sequence = exec.scenario.iterationInTest;
  const operation = operations[sequence % operations.length];
  const response = http.get(`${TARGET}${operation.path}`, {
    headers: {
      Authorization: `Bearer ${TOKENS[sequence % TOKENS.length]}`,
      ...(operation.kind === 'report' ? { 'If-None-Match': ETAG } : {}),
    },
    responseType: 'text', tags: { endpoint: operation.kind },
  });
  latency.add(response.timings.duration);
  server5xx.add(response.status >= 500 && response.status !== 503);
  rateLimited.add(response.status === 429);
  const bytes = String(response.body || '').length;
  let valid;
  if (operation.kind === 'report') {
    valid = validConditionalReport(
      response.status,
      bytes,
      String(response.headers.ETag || response.headers.Etag || response.headers.etag || ''),
      String(response.headers['Retry-After'] || response.headers['retry-after'] || ''),
    );
  } else {
    let body;
    try { body = response.json(); } catch (_) { body = null; }
    valid = response.status === 200 && validIncidentPage(body, operation.after, bytes, operation.ascending);
  }
  invalid.add(!valid);
}

export function health() {
  const response = http.get(`${TARGET}/healthz`, { responseType: 'text' });
  server5xx.add(response.status >= 500);
  invalid.add(response.status !== 200 || response.body !== 'ok');
}
